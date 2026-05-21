//! Document ingestion and knowledge extraction for the OpenHuman memory system.
//!
//! This module provides the pipeline for taking raw unstructured text and
//! transforming it into structured memory. The process includes:
//! 1. **Chunking**: Splitting the document into manageable pieces.
//! 2. **Structured Extraction**: Using regex-based rules to identify known patterns
//!    (e.g., email headers, specific project labels).
//! 3. **Heuristic Extraction**: Using rule-based parsing to identify entities
//!    and their relationships.
//! 4. **Aggregation**: Resolving aliases, merging duplicates, and normalizing names.
//! 5. **Persistence**: Upserting the document, text chunks, and graph relations into
//!    the memory store.

pub mod llm_extract;
mod parse;
mod regex;
mod rules;
mod types;

pub mod queue;
pub mod state;

pub use llm_extract::{
    ChatBackedLlmGraphExtractor, LlmEntitySpec, LlmGraphExtraction, LlmGraphExtractor,
    LlmRelationSpec,
};
pub use queue::{IngestionJob, IngestionQueue};
pub use state::{IngestionState, IngestionStatusSnapshot};
pub use types::{
    ExtractedEntity, ExtractedRelation, ExtractionMode, MemoryIngestionConfig,
    MemoryIngestionRequest, MemoryIngestionResult, DEFAULT_MEMORY_EXTRACTION_MODEL,
};

use std::sync::Arc;

use parse::{enrich_document_metadata, parse_document};
use serde_json::json;
use types::ParsedIngestion;

use crate::openhuman::memory::store::types::NamespaceDocumentInput;
use crate::openhuman::memory::UnifiedMemory;

impl UnifiedMemory {
    /// Run the full ingestion pipeline for a document with the legacy
    /// heuristic-only path — no LLM extractor wired. Equivalent to
    /// [`ingest_document_with_extractor`] with `extractor = None`.
    pub async fn ingest_document(
        &self,
        request: MemoryIngestionRequest,
    ) -> Result<MemoryIngestionResult, String> {
        self.ingest_document_with_extractor(request, None).await
    }

    /// Run the full ingestion pipeline for a document: parse + chunk + extract
    /// entities/relations (optionally LLM-driven), upsert the document row +
    /// vector chunks, and write the extracted relations into the namespace
    /// graph.
    ///
    /// `extractor` controls the LLM step. When `Some`, the parser runs both
    /// the heuristic extractor and the LLM extractor and merges their outputs
    /// (see [`super::ingestion::parse::parse_document`] for the order of
    /// operations + soft-fallback semantics). When `None`, only the
    /// heuristic runs — same as the legacy [`ingest_document`].
    pub async fn ingest_document_with_extractor(
        &self,
        request: MemoryIngestionRequest,
        extractor: Option<Arc<dyn LlmGraphExtractor>>,
    ) -> Result<MemoryIngestionResult, String> {
        let parsed = parse_document(
            &request.document.content,
            &request.document.title,
            &request.config,
            extractor.as_deref().map(|e| e as &dyn LlmGraphExtractor),
        )
        .await;
        let (enriched_input, tags) =
            enrich_document_metadata(&request.document, &parsed, &request.config);
        let namespace = Self::sanitize_namespace(&enriched_input.namespace);
        let document_id = self.upsert_document(enriched_input).await?;

        self.upsert_graph_relations(&namespace, &document_id, &parsed, &request.config)
            .await?;

        Ok(build_result(
            document_id,
            namespace,
            &request.config,
            parsed,
            tags,
        ))
    }

    /// Extract entities/relations (heuristic only) and write them to the
    /// graph for a document that has already been stored via
    /// [`upsert_document`]. Equivalent to
    /// [`extract_graph_with_extractor`] with `extractor = None`.
    pub async fn extract_graph(
        &self,
        document_id: &str,
        document: &NamespaceDocumentInput,
        config: &MemoryIngestionConfig,
    ) -> Result<MemoryIngestionResult, String> {
        self.extract_graph_with_extractor(document_id, document, config, None)
            .await
    }

    /// Extract entities/relations (optionally LLM-driven) and write them to
    /// the graph for a document that has already been stored.
    ///
    /// This avoids the redundant second upsert that would happen if the
    /// background ingestion queue called [`ingest_document`] on an already-
    /// persisted document.
    pub async fn extract_graph_with_extractor(
        &self,
        document_id: &str,
        document: &NamespaceDocumentInput,
        config: &MemoryIngestionConfig,
        extractor: Option<Arc<dyn LlmGraphExtractor>>,
    ) -> Result<MemoryIngestionResult, String> {
        let parsed = parse_document(
            &document.content,
            &document.title,
            config,
            extractor.as_deref().map(|e| e as &dyn LlmGraphExtractor),
        )
        .await;
        let namespace = Self::sanitize_namespace(&document.namespace);

        self.upsert_graph_relations(&namespace, document_id, &parsed, config)
            .await?;

        let (_, tags) = enrich_document_metadata(document, &parsed, config);

        Ok(build_result(
            document_id.to_string(),
            namespace,
            config,
            parsed,
            tags,
        ))
    }

    /// Clear existing relations for the document then upsert all extracted
    /// relations into the namespace graph.
    async fn upsert_graph_relations(
        &self,
        namespace: &str,
        document_id: &str,
        parsed: &ParsedIngestion,
        config: &MemoryIngestionConfig,
    ) -> Result<(), String> {
        self.graph_remove_document_namespace(namespace, document_id)
            .await?;

        for relation in &parsed.relations {
            let chunk_ids = relation
                .chunk_ids
                .iter()
                .filter_map(|chunk_id| chunk_id.strip_prefix("chunk:"))
                .map(|chunk_index| format!("{document_id}:{chunk_index}"))
                .collect::<Vec<_>>();

            let attrs = json!({
                "source": "ingestion",
                "model_name": parsed
                    .model_label
                    .clone()
                    .unwrap_or_else(|| config.model_name.clone()),
                "extraction_mode": config.extraction_mode.as_str(),
                "extraction_backend": parsed.extraction_backend.clone(),
                "graph_extraction": config.graph_extraction.as_str(),
                "confidence": relation.confidence,
                "evidence_count": relation.evidence_count,
                "order_index": relation.order_index,
                "document_id": document_id,
                "document_ids": [document_id],
                "chunk_ids": chunk_ids,
                "entity_types": {
                    "subject": relation.subject_type,
                    "object": relation.object_type,
                },
                "metadata": relation.metadata,
            });

            self.graph_upsert_namespace(
                namespace,
                &relation.subject,
                &relation.predicate,
                &relation.object,
                &attrs,
            )
            .await?;
        }

        Ok(())
    }
}

/// Shared `MemoryIngestionResult` constructor used by both
/// `ingest_document_with_extractor` and `extract_graph_with_extractor`.
///
/// Lifts the resolved model identifier and backend label from
/// `ParsedIngestion` into the result so callers see "extracted via
/// `<model>`" instead of the legacy `heuristic-only` literal, and so
/// the UI / activity log can render the backend.
fn build_result(
    document_id: String,
    namespace: String,
    config: &MemoryIngestionConfig,
    parsed: ParsedIngestion,
    tags: Vec<String>,
) -> MemoryIngestionResult {
    MemoryIngestionResult {
        document_id,
        namespace,
        model_name: parsed
            .model_label
            .clone()
            .unwrap_or_else(|| config.model_name.clone()),
        extraction_backend: parsed.extraction_backend.clone(),
        extraction_mode: config.extraction_mode.as_str().to_string(),
        chunk_count: parsed.chunk_count,
        entity_count: parsed.entities.len(),
        relation_count: parsed.relations.len(),
        preference_count: parsed.preference_count,
        decision_count: parsed.decision_count,
        tags,
        entities: parsed.entities,
        relations: parsed.relations,
    }
}

#[cfg(test)]
mod tests;
