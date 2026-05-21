//! # Memory Client
//!
//! High-level client interface for interacting with the OpenHuman memory system.
//!
//! The `MemoryClient` provides a simplified API for storing and retrieving
//! information from the memory store, handling background tasks like graph
//! extraction and embedding generation. It primarily acts as a wrapper around
//! `UnifiedMemory`.

use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

use crate::openhuman::config::{Config, GraphExtractionMode};
use crate::openhuman::embeddings::{self, EmbeddingProvider, NoopEmbedding};
use crate::openhuman::memory::ingestion::queue as ingestion_queue;
use crate::openhuman::memory::ingestion::{
    ChatBackedLlmGraphExtractor, IngestionJob, IngestionQueue, IngestionState, LlmGraphExtractor,
    MemoryIngestionConfig, MemoryIngestionRequest, MemoryIngestionResult,
};
use crate::openhuman::memory::tree::chat::{build_chat_provider_for_role, ChatProvider};
use crate::openhuman::memory::store::factories::effective_embedding_settings;
use crate::openhuman::memory::store::types::{
    NamespaceDocumentInput, NamespaceMemoryHit, NamespaceRetrievalContext,
};
use crate::openhuman::memory::store::unified::UnifiedMemory;

/// Reference-counted handle to a `MemoryClient`.
pub type MemoryClientRef = Arc<MemoryClient>;

/// Thread-safe container for an optional `MemoryClientRef`.
///
/// Used for global state management where the memory client may or may not
/// be initialized.
pub struct MemoryState(pub std::sync::Mutex<Option<MemoryClientRef>>);

/// SQLite-backed memory client rooted at the user's workspace directory.
///
/// Storage (documents, vectors, graph) remains on-device via [`UnifiedMemory`].
/// Embedding generation is delegated to whichever provider the
/// [`MemoryConfig.embedding_provider`](crate::openhuman::config::MemoryConfig)
/// resolves to — cloud (OpenHuman backend, the default returned by
/// [`crate::openhuman::embeddings::default_embedding_provider`]) or local Ollama
/// when explicitly opted into. The cloud embedder resolves its session JWT
/// lazily, so an unauthenticated session will surface as a clear error on the
/// first `embed` call rather than at client construction.
///
/// Callers that need a non-default embedder should construct the underlying
/// store via [`crate::openhuman::memory::create_memory_with_storage_and_routes`]
/// (or [`crate::openhuman::memory::create_memory_with_local_ai`]) with the
/// appropriate `MemoryConfig.embedding_provider`.
#[derive(Clone)]
pub struct MemoryClient {
    /// The underlying memory implementation.
    inner: Arc<UnifiedMemory>,
    /// Queue for background ingestion tasks (e.g., entity extraction).
    ingestion_queue: IngestionQueue,
    /// LLM-driven entity / relation extractor for the namespace graph.
    /// Built lazily from `Config::memory.graph_extraction` +
    /// `memory_provider` at construction time. `None` when the user has
    /// disabled it (`graph_extraction = "heuristic"`), when no chat
    /// provider is wired, or when the chat-provider build fails. The
    /// ingestion pipeline soft-falls back to heuristic on any LLM-side
    /// failure, so `None` simply means we don't even try.
    graph_extractor: Option<Arc<dyn LlmGraphExtractor>>,
    /// Strategy from `Config::memory.graph_extraction`. Stored so the
    /// per-job `MemoryIngestionConfig` can carry the resolved mode
    /// downstream into `parse_document` (the parser respects this knob).
    graph_extraction_mode: GraphExtractionMode,
}

impl MemoryClient {
    /// Returns a handle to the underlying SQLite connection for direct
    /// profile-facet writes via
    /// [`crate::openhuman::memory::store::unified::profile::profile_upsert`].
    ///
    /// Intentionally `pub(crate)` — external consumers should use the
    /// higher-level `MemoryClient` API; this escape hatch exists so
    /// in-crate subsystems (composio providers, archivist, learning
    /// hooks) can write structured profile facets without an additional
    /// round-trip through the ingestion queue.
    pub(crate) fn profile_conn(&self) -> std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>> {
        std::sync::Arc::clone(&self.inner.conn)
    }

    /// Returns an `Arc<dyn Memory>` handle backed by the same
    /// [`UnifiedMemory`] this client wraps. Used by sub-systems that
    /// want to build on top of the `Memory` trait (e.g. the
    /// tool-scoped memory layer) without depending on the concrete
    /// `MemoryClient` type or holding a reference to it.
    pub fn memory_handle(&self) -> Arc<dyn crate::openhuman::memory::Memory> {
        Arc::clone(&self.inner) as Arc<dyn crate::openhuman::memory::Memory>
    }

    /// Create a new local memory client using the default `.openhuman` directory.
    ///
    /// # Errors
    ///
    /// Returns an error string if the home directory cannot be resolved or if
    /// initialization fails.
    pub fn new_local() -> Result<Self, String> {
        let workspace_dir = crate::openhuman::config::default_root_openhuman_dir()
            .map_err(|e| e.to_string())?
            .join("workspace");
        Self::from_workspace_dir(workspace_dir)
    }

    /// Create a new memory client from a specific workspace directory.
    ///
    /// # Arguments
    ///
    /// * `workspace_dir` - The path where memory databases and assets are stored.
    ///
    /// # Errors
    ///
    /// Returns an error string if the directory cannot be created or if the
    /// `UnifiedMemory` or `IngestionQueue` fails to start.
    pub fn from_workspace_dir(workspace_dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&workspace_dir)
            .map_err(|e| format!("Create workspace dir {}: {e}", workspace_dir.display()))?;

        // Local-OAuth fork: the previous behaviour was to hard-code
        // `embeddings::default_embedding_provider()` here — the dead
        // OpenHuman cloud embedder. Every vault ingest, document
        // upsert, and memory write would call `embedder.embed_one()`,
        // fail (no backend), get silently swallowed by `.ok()` in
        // `upsert_document` and `vector_chunks` rows were inserted
        // with NULL embeddings. Symptom: the user reports "chunks are
        // present but no vectors", semantic search returns nothing.
        //
        // Read the workspace's `config.toml` synchronously, derive the
        // embedder from the unified `embeddings_provider` workload
        // field (or the legacy `memory.embedding_provider`), and fall
        // back to `NoopEmbedding` with a LOUD warning when
        // construction fails — never to the dead cloud embedder.
        let (embedder, loaded_config) = build_workspace_embedder_and_config(&workspace_dir);

        // Create the underlying UnifiedMemory instance.
        let memory =
            UnifiedMemory::new(&workspace_dir, embedder, None).map_err(|e| format!("{e}"))?;
        let inner = Arc::new(memory);

        // Start the background worker for document ingestion and graph extraction.
        // The worker shares its IngestionState with the synchronous ingest path
        // below so all ingestion is singleton-serialised.
        let ingestion_queue =
            ingestion_queue::start_worker_with_state(Arc::clone(&inner), IngestionState::new());

        // Build the LLM-driven namespace-graph extractor when the user
        // has configured `memory_provider` and not disabled the mode.
        // Failure to construct (no provider configured, bad endpoint,
        // etc.) is non-fatal — the ingestion pipeline soft-falls back
        // to heuristic-only.
        let graph_extraction_mode = loaded_config.memory.graph_extraction;
        let graph_extractor = build_graph_extractor(&loaded_config, graph_extraction_mode);

        Ok(Self {
            inner,
            ingestion_queue,
            graph_extractor,
            graph_extraction_mode,
        })
    }

    /// Store a document in a specific namespace.
    ///
    /// This method performs an "upsert" (update or insert). It immediately
    /// persists the document and then enqueues a background job for graph
    /// extraction (entities and relations).
    ///
    /// # Arguments
    ///
    /// * `input` - The document content and metadata.
    ///
    /// # Returns
    ///
    /// The unique ID of the stored document.
    pub async fn put_doc(&self, input: NamespaceDocumentInput) -> Result<String, String> {
        let document_id = self.inner.upsert_document(input.clone()).await?;

        // Enqueue background graph extraction so entities/relations are
        // extracted without blocking the caller. The document is already
        // persisted — extract_graph will not upsert again.
        self.ingestion_queue.submit(IngestionJob {
            document_id: document_id.clone(),
            document: input,
            config: self.default_ingestion_config(),
            extractor: self.graph_extractor.clone(),
        });

        Ok(document_id)
    }

    /// Store a document (DB row + markdown file) without vector embedding or
    /// graph extraction.  Use this for high-frequency, ephemeral writes where
    /// the full pipeline would be too expensive (e.g. screen-intelligence
    /// snapshots).  The document is still searchable by metadata/FTS but will
    /// not appear in semantic vector queries or the knowledge graph.
    pub async fn put_doc_light(&self, input: NamespaceDocumentInput) -> Result<String, String> {
        self.inner.upsert_document_metadata_only(input).await
    }

    /// Perform a full ingestion (chunking, embedding, extraction) synchronously.
    ///
    /// Unlike `put_doc`, this waits for the entire process to complete.
    /// Serialised against the background worker via the shared
    /// [`IngestionState`] singleton lock — only one ingestion runs at a time.
    pub async fn ingest_doc(
        &self,
        mut request: MemoryIngestionRequest,
    ) -> Result<MemoryIngestionResult, String> {
        let state = self.ingestion_queue.state();
        let _guard = state.acquire().await;

        let title = request.document.title.clone();
        let namespace = request.document.namespace.clone();
        // Synthetic id until upsert assigns one — purely for the snapshot.
        let placeholder_id = format!("sync:{title}");

        let queue_depth = state.snapshot().queue_depth;
        state.mark_running(&placeholder_id, &title, &namespace);
        crate::core::event_bus::publish_global(
            crate::core::event_bus::DomainEvent::MemoryIngestionStarted {
                document_id: placeholder_id.clone(),
                title,
                namespace: namespace.clone(),
                queue_depth,
            },
        );

        // Inherit the client's configured graph_extraction mode unless
        // the caller explicitly overrode it on the request. The default
        // for a fresh `MemoryIngestionConfig` is `Auto`, which is also
        // the safe default — only override when the caller's config
        // matches `MemoryIngestionConfig::default()` (signal: model_name
        // is still the literal heuristic-only and graph_extraction is
        // `Auto`).
        if request.config.graph_extraction == GraphExtractionMode::default()
            && self.graph_extraction_mode != GraphExtractionMode::default()
        {
            request.config.graph_extraction = self.graph_extraction_mode;
        }

        let started = std::time::Instant::now();
        let outcome = self
            .inner
            .ingest_document_with_extractor(request, self.graph_extractor.clone())
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let success = outcome.is_ok();

        // Use the same placeholder id as the matching MemoryIngestionStarted
        // event so subscribers can correlate start/complete pairs. The real
        // upstream-assigned document id is available on `Ok(outcome)` for
        // callers that need it.
        state.mark_completed(
            &placeholder_id,
            success,
            chrono::Utc::now().timestamp_millis(),
        );
        crate::core::event_bus::publish_global(
            crate::core::event_bus::DomainEvent::MemoryIngestionCompleted {
                document_id: placeholder_id,
                namespace,
                success,
                elapsed_ms,
                queue_depth: state.snapshot().queue_depth,
            },
        );

        outcome
    }

    /// Returns the shared ingestion state — singleton lock + status snapshot.
    /// Used by the `openhuman.memory_ingestion_status` RPC handler.
    pub fn ingestion_state(&self) -> IngestionState {
        self.ingestion_queue.state()
    }

    /// Specialized method for syncing skill data into memory.
    ///
    /// Maps generic skill/integration fields into the `NamespaceDocumentInput` structure.
    #[allow(clippy::too_many_arguments)]
    pub async fn store_skill_sync(
        &self,
        skill_id: &str,
        _integration_id: &str,
        title: &str,
        content: &str,
        source_type: Option<String>,
        metadata: Option<serde_json::Value>,
        priority: Option<String>,
        _created_at: Option<f64>,
        _updated_at: Option<f64>,
        document_id: Option<String>,
    ) -> Result<(), String> {
        let namespace = format!("skill-{}", skill_id.trim());
        let input = NamespaceDocumentInput {
            namespace,
            key: title.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            source_type: source_type.unwrap_or_else(|| "doc".to_string()),
            priority: priority.unwrap_or_else(|| "medium".to_string()),
            tags: Vec::new(),
            metadata: metadata.unwrap_or_else(|| json!({})),
            category: "core".to_string(),
            session_id: None,
            document_id,
        };

        let doc_id = self.inner.upsert_document(input.clone()).await?;

        // Enqueue background graph extraction.
        self.ingestion_queue.submit(IngestionJob {
            document_id: doc_id,
            document: input,
            config: self.default_ingestion_config(),
            extractor: self.graph_extractor.clone(),
        });

        Ok(())
    }

    /// Build a `MemoryIngestionConfig` honouring the client's resolved
    /// `graph_extraction` mode. Used by enqueue-style call sites
    /// (`put_doc`, `store_skill_sync`) where the caller doesn't supply
    /// its own config.
    fn default_ingestion_config(&self) -> MemoryIngestionConfig {
        MemoryIngestionConfig {
            graph_extraction: self.graph_extraction_mode,
            ..MemoryIngestionConfig::default()
        }
    }

    /// List documents in a namespace (or all namespaces if `None`).
    pub async fn list_documents(
        &self,
        namespace: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        self.inner.list_documents(namespace).await
    }

    /// List all unique namespaces in the memory store.
    pub async fn list_namespaces(&self) -> Result<Vec<String>, String> {
        self.inner.list_namespaces().await
    }

    /// Delete a specific document by its ID and namespace.
    pub async fn delete_document(
        &self,
        namespace: &str,
        document_id: &str,
    ) -> Result<serde_json::Value, String> {
        self.inner.delete_document(namespace, document_id).await
    }

    /// Clear all documents and data within a specific namespace.
    pub async fn clear_namespace(&self, namespace: &str) -> Result<(), String> {
        self.inner.clear_namespace(namespace).await
    }

    /// Clear memory associated with a specific skill.
    pub async fn clear_skill_memory(
        &self,
        skill_id: &str,
        _integration_id: &str,
    ) -> Result<(), String> {
        let namespace = format!("skill-{}", skill_id.trim());
        let docs = self.list_documents(Some(&namespace)).await?;
        let items = docs
            .get("documents")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in items {
            if let Some(document_id) = item.get("documentId").and_then(serde_json::Value::as_str) {
                let _ = self.delete_document(&namespace, document_id).await?;
            }
        }
        Ok(())
    }

    /// Query a namespace for context using natural language.
    ///
    /// Returns a formatted string containing relevant text chunks and context.
    pub async fn query_namespace(
        &self,
        namespace: &str,
        query: &str,
        max_chunks: u32,
    ) -> Result<String, String> {
        self.inner
            .query_namespace_context(namespace, query, max_chunks)
            .await
    }

    /// Query a namespace and return raw context data (hits, relations, etc.).
    pub async fn query_namespace_context_data(
        &self,
        namespace: &str,
        query: &str,
        max_chunks: u32,
    ) -> Result<NamespaceRetrievalContext, String> {
        self.inner
            .query_namespace_context_data(namespace, query, max_chunks)
            .await
    }

    /// Recall recent context from a namespace without a specific query.
    pub async fn recall_namespace(
        &self,
        namespace: &str,
        max_chunks: u32,
    ) -> Result<Option<String>, String> {
        self.inner
            .recall_namespace_context(namespace, max_chunks)
            .await
    }

    /// Recall raw context data from a namespace without a specific query.
    pub async fn recall_namespace_context_data(
        &self,
        namespace: &str,
        max_chunks: u32,
    ) -> Result<NamespaceRetrievalContext, String> {
        self.inner
            .recall_namespace_context_data(namespace, max_chunks)
            .await
    }

    /// Recall a specific number of recent memories (hits) from a namespace.
    pub async fn recall_namespace_memories(
        &self,
        namespace: &str,
        limit: u32,
    ) -> Result<Vec<NamespaceMemoryHit>, String> {
        self.inner.recall_namespace_memories(namespace, limit).await
    }

    /// Store a key-value pair in a namespace (or global if `None`).
    pub async fn kv_set(
        &self,
        namespace: Option<&str>,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), String> {
        match namespace {
            Some(ns) => self.inner.kv_set_namespace(ns, key, value).await,
            None => self.inner.kv_set_global(key, value).await,
        }
    }

    /// Retrieve a key-value pair.
    pub async fn kv_get(
        &self,
        namespace: Option<&str>,
        key: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        match namespace {
            Some(ns) => self.inner.kv_get_namespace(ns, key).await,
            None => self.inner.kv_get_global(key).await,
        }
    }

    /// Delete a key-value pair.
    pub async fn kv_delete(&self, namespace: Option<&str>, key: &str) -> Result<bool, String> {
        match namespace {
            Some(ns) => self.inner.kv_delete_namespace(ns, key).await,
            None => self.inner.kv_delete_global(key).await,
        }
    }

    /// List all key-value pairs in a namespace.
    pub async fn kv_list_namespace(
        &self,
        namespace: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        self.inner.kv_list_namespace(namespace).await
    }

    /// Upsert a relationship in the knowledge graph.
    pub async fn graph_upsert(
        &self,
        namespace: Option<&str>,
        subject: &str,
        predicate: &str,
        object: &str,
        attrs: &serde_json::Value,
    ) -> Result<(), String> {
        match namespace {
            Some(ns) => {
                self.inner
                    .graph_upsert_namespace(ns, subject, predicate, object, attrs)
                    .await
            }
            None => {
                self.inner
                    .graph_upsert_global(subject, predicate, object, attrs)
                    .await
            }
        }
    }

    /// Query relationships in the knowledge graph using optional filters.
    ///
    /// When `namespace` is `None`, returns relations from **all** namespaces
    /// plus the global graph, so ingested data is always surfaced in the UI.
    pub async fn graph_query(
        &self,
        namespace: Option<&str>,
        subject: Option<&str>,
        predicate: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        match namespace {
            Some(ns) => {
                self.inner
                    .graph_query_namespace(ns, subject, predicate)
                    .await
            }
            None => self.inner.graph_query_all(subject, predicate).await,
        }
    }
}

/// Build the embedder for `MemoryClient::from_workspace_dir` by reading
/// the workspace's `config.toml` synchronously and resolving the
/// embedding provider via the same `effective_embedding_settings`
/// logic the agent harness uses.
///
/// Failure modes (each falls back to `NoopEmbedding` with a LOUD
/// warning — never to the dead OpenHuman cloud embedder):
/// - config.toml missing → Config::default() is used.
/// - config.toml parse error → Config::default() with a warning.
/// - embedder construction fails (e.g. unknown provider string,
///   bad endpoint) → NoopEmbedding with a warning.
///
/// The fallback to `NoopEmbedding` keeps memory writes succeeding
/// (chunks land in the namespace store, FTS / metadata search still
/// works) while semantic search degrades to no-op rather than
/// silently inserting NULL embeddings into `vector_chunks`. That
/// silent-NULL behaviour was the original symptom: chunks present
/// but no vectors.
fn build_workspace_embedder_and_config(
    workspace_dir: &std::path::Path,
) -> (Arc<dyn EmbeddingProvider>, Config) {
    let config = load_workspace_config(workspace_dir);
    let embedder = build_workspace_embedder_from_config(&config);
    (embedder, config)
}

/// Synchronously load `config.toml` for the workspace, falling back to
/// `Config::default()` on missing / parse-failure / unknown-field rows.
/// Used by [`build_workspace_embedder_and_config`] and the LLM graph
/// extractor builder; both want the same resolved `Config` so the
/// `memory_provider` routing the chat-provider factory does aligns with
/// the embedder choice.
fn load_workspace_config(workspace_dir: &std::path::Path) -> Config {
    let candidates: Vec<std::path::PathBuf> = std::iter::empty()
        .chain(
            workspace_dir
                .parent()
                .map(|p| p.join("config.toml"))
                .into_iter(),
        )
        .chain(std::iter::once(workspace_dir.join("config.toml")))
        .collect();
    let mut config = Config::default();
    let mut loaded_from: Option<std::path::PathBuf> = None;
    for candidate in &candidates {
        match std::fs::read_to_string(candidate) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => {
                    config = cfg;
                    loaded_from = Some(candidate.clone());
                    break;
                }
                Err(e) => {
                    log::warn!(
                        "[memory:embedder] failed to parse {} ({e}); trying next candidate",
                        candidate.display()
                    );
                }
            },
            Err(_) => {
                // Not present — try the next candidate.
            }
        }
    }
    if loaded_from.is_none() {
        log::debug!(
            "[memory:embedder] no config.toml found near workspace_dir={} \
             (tried {:?}); using Config::default()",
            workspace_dir.display(),
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>(),
        );
    } else {
        log::debug!(
            "[memory:embedder] loaded config from {}",
            loaded_from.as_ref().unwrap().display()
        );
    }
    config
}

fn build_workspace_embedder_from_config(config: &Config) -> Arc<dyn EmbeddingProvider> {
    let local_embedding_model = config.workload_local_model("embeddings");
    let (provider, model, dims) =
        effective_embedding_settings(&config.memory, local_embedding_model.as_deref());

    log::info!(
        "[memory:embedder] resolved provider={provider} model={model} dims={dims} \
         (embeddings_provider={:?} memory.embedding_provider={:?})",
        config.embeddings_provider,
        config.memory.embedding_provider,
    );

    match embeddings::create_embedding_provider(&provider, &model, dims) {
        Ok(boxed) => Arc::from(boxed),
        Err(e) => {
            log::warn!(
                "[memory:embedder] failed to construct embedder provider={provider} \
                 model={model} dims={dims} err={e}; falling back to NoopEmbedding. \
                 Configure a valid embeddings provider in Settings → AI."
            );
            Arc::new(NoopEmbedding)
        }
    }
}

/// Build the LLM-driven namespace graph extractor for this workspace's
/// configuration. Returns `None` when:
/// - `mode = GraphExtractionMode::Heuristic` (user explicitly opted out)
/// - the chat-provider factory fails (no `memory_provider` configured,
///   bad endpoint, unknown slug) — the pipeline soft-falls back so this
///   isn't fatal, just diagnostic.
///
/// When `mode = Auto`, the absence of a `memory_provider` is silent —
/// we simply don't wire an extractor and the heuristic path runs.
/// When `mode = Llm`, the absence is logged at warn level because the
/// user explicitly asked for LLM extraction.
fn build_graph_extractor(
    config: &Config,
    mode: GraphExtractionMode,
) -> Option<Arc<dyn LlmGraphExtractor>> {
    if !mode.wants_llm() {
        log::info!(
            "[memory:llm_extract] graph_extraction={} — heuristic-only path active",
            mode.as_str()
        );
        return None;
    }
    // Reuse the memory-tree chat factory: it already knows how to route
    // a `memory_provider = "ollama:<m>"` to a local OllamaChatProvider
    // and a `memory_provider = "openai:<m>"` (or unset → first non-dead
    // cloud_providers row) to the WorkloadChatProvider.
    let chat_provider: Arc<dyn ChatProvider> =
        match build_chat_provider_for_role(config, "memory", 30_000) {
            Ok(provider) => provider,
            Err(e) => {
                if mode == GraphExtractionMode::Llm {
                    log::warn!(
                        "[memory:llm_extract] graph_extraction=llm requested but chat \
                         provider build failed: {e:#}. Heuristic-only path will be used."
                    );
                } else {
                    log::info!(
                        "[memory:llm_extract] graph_extraction=auto but no chat provider \
                         could be built ({e:#}); heuristic-only path active."
                    );
                }
                return None;
            }
        };
    let provider_name = chat_provider.name().to_string();
    log::info!(
        "[memory:llm_extract] graph_extraction={} — LLM extractor wired via {}",
        mode.as_str(),
        provider_name,
    );
    Some(Arc::new(ChatBackedLlmGraphExtractor::new(
        chat_provider,
        provider_name,
    )))
}

// Legacy single-return helper kept for any external callers that don't
// want the resolved `Config` back. Forwards to the new split helpers.
#[allow(dead_code)]
fn build_workspace_embedder(workspace_dir: &std::path::Path) -> Arc<dyn EmbeddingProvider> {
    build_workspace_embedder_and_config(workspace_dir).0
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
