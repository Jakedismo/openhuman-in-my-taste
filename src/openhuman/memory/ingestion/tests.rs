//! Tests for the ingestion pipeline — `parse_document`, regex extraction,
//! and `UnifiedMemory::ingest_document` end-to-end.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;

use crate::openhuman::config::GraphExtractionMode;
use crate::openhuman::embeddings::NoopEmbedding;
use crate::openhuman::memory::ingestion::llm_extract::{
    LlmGraphExtraction, LlmGraphExtractor, LlmRelationSpec,
};
use crate::openhuman::memory::{
    MemoryIngestionConfig, MemoryIngestionRequest, NamespaceDocumentInput, UnifiedMemory,
};

/// Test config for the heuristic-only ingestion pipeline.
fn ci_safe_config() -> MemoryIngestionConfig {
    MemoryIngestionConfig::default()
}

fn fixture(path: &str) -> String {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(
        base.join("tests")
            .join("fixtures")
            .join("ingestion")
            .join(path),
    )
    .expect("fixture should load")
}

#[tokio::test]
async fn gmail_fixture_ingestion_recovers_required_signals() {
    let tmp = TempDir::new().unwrap();
    let memory = UnifiedMemory::new(tmp.path(), Arc::new(NoopEmbedding), None).unwrap();
    let result = memory
        .ingest_document(MemoryIngestionRequest {
            document: NamespaceDocumentInput {
                namespace: "skill-gmail".to_string(),
                key: "gmail-thread-memory-integration".to_string(),
                title: "Memory integration plan for OpenHuman desktop".to_string(),
                content: fixture("gmail_thread_example.txt"),
                source_type: "gmail".to_string(),
                priority: "high".to_string(),
                tags: Vec::new(),
                metadata: json!({}),
                category: "core".to_string(),
                session_id: None,
                document_id: None,
            },
            config: ci_safe_config(),
        })
        .await
        .unwrap();

    assert!(result
        .entities
        .iter()
        .any(|entity| entity.name == "SANIL JAIN"));
    assert!(result
        .entities
        .iter()
        .any(|entity| entity.name == "RAVI KULKARNI"));
    assert!(result
        .entities
        .iter()
        .any(|entity| entity.name == "ASHA MEHTA"));
    assert!(result
        .entities
        .iter()
        .any(|entity| entity.name == "OPENHUMAN"));
    assert!(result
        .relations
        .iter()
        .any(|relation| relation.subject == "OPENHUMAN"
            && relation.predicate == "USES"
            && relation.object.contains("JSON-RPC")));
    assert!(result
        .relations
        .iter()
        .any(|relation| relation.subject == "RAVI KULKARNI" && relation.predicate == "OWNS"));
    assert!(result.preference_count >= 1);
    assert!(result.decision_count >= 1);

    let context = memory
        .query_namespace_context_data("skill-gmail", "who owns the rust memory api alignment", 5)
        .await
        .unwrap();
    assert!(context
        .hits
        .iter()
        .flat_map(|hit| hit.supporting_relations.iter())
        .any(|relation| relation.subject == "RAVI KULKARNI" && relation.predicate == "OWNS"));

    let recall = memory
        .recall_namespace_context_data("skill-gmail", 5)
        .await
        .unwrap();
    assert!(!recall.context_text.is_empty());
    assert!(recall
        .hits
        .iter()
        .any(|hit| hit.content.contains("OpenHuman") || hit.content.contains("JSON-RPC")));
    assert!(recall
        .hits
        .iter()
        .any(|hit| !hit.supporting_relations.is_empty()));

    let memories = memory
        .recall_namespace_memories("skill-gmail", 5)
        .await
        .unwrap();
    assert!(memories.iter().any(|hit| hit.content.contains("JSON-RPC")));
    assert!(memories
        .iter()
        .any(|hit| matches!(hit.kind, crate::openhuman::memory::MemoryItemKind::Document)));
    assert!(memories
        .iter()
        .any(|hit| !hit.supporting_relations.is_empty()));
}

#[tokio::test]
async fn notion_fixture_ingestion_recovers_required_signals() {
    let tmp = TempDir::new().unwrap();
    let memory = UnifiedMemory::new(tmp.path(), Arc::new(NoopEmbedding), None).unwrap();
    let result = memory
        .ingest_document(MemoryIngestionRequest {
            document: NamespaceDocumentInput {
                namespace: "skill-notion".to_string(),
                key: "notion-roadmap-memory-layer".to_string(),
                title: "OpenHuman Memory Layer Roadmap".to_string(),
                content: fixture("notion_page_example.txt"),
                source_type: "notion".to_string(),
                priority: "high".to_string(),
                tags: Vec::new(),
                metadata: json!({}),
                category: "core".to_string(),
                session_id: None,
                document_id: None,
            },
            config: ci_safe_config(),
        })
        .await
        .unwrap();

    assert!(result
        .entities
        .iter()
        .any(|entity| entity.name == "OPENHUMAN"));
    assert!(result
        .entities
        .iter()
        .any(|entity| entity.name == "SANIL JAIN"));
    assert!(result
        .relations
        .iter()
        .any(|relation| relation.subject == "OPENHUMAN"
            && relation.predicate == "USES"
            && relation.object.contains("JSON-RPC")));
    assert!(result
        .relations
        .iter()
        .any(|relation| relation.subject == "CORE CONTRACT LOCKED"
            && relation.predicate == "HAS_DEADLINE"));
    assert!(result
        .relations
        .iter()
        .any(|relation| relation.subject == "SANIL JAIN" && relation.predicate == "PREFERS"));
    assert!(result.preference_count >= 1);
    assert!(result.decision_count >= 1);

    let graph_rows = memory
        .graph_query_namespace("skill-notion", Some("OPENHUMAN"), Some("USES"))
        .await
        .unwrap();
    assert!(!graph_rows.is_empty());

    let context = memory
        .query_namespace_context_data(
            "skill-notion",
            "who prefers core-first delivery over ui-first delivery",
            5,
        )
        .await
        .unwrap();
    assert!(context
        .hits
        .iter()
        .flat_map(|hit| hit.supporting_relations.iter())
        .any(|relation| relation.subject == "SANIL JAIN" && relation.predicate == "PREFERS"));

    let recall = memory
        .recall_namespace_context_data("skill-notion", 5)
        .await
        .unwrap();
    assert!(!recall.context_text.is_empty());
    assert!(recall
        .hits
        .iter()
        .any(|hit| hit.content.contains("OpenHuman")));

    let memories = memory
        .recall_namespace_memories("skill-notion", 5)
        .await
        .unwrap();
    assert!(memories
        .iter()
        .any(|hit| hit.content.contains("OpenHuman") || hit.content.contains("core-first")));
    assert!(memories
        .iter()
        .any(|hit| matches!(hit.kind, crate::openhuman::memory::MemoryItemKind::Document)));
    assert!(memories
        .iter()
        .any(|hit| !hit.supporting_relations.is_empty()));
}

// ── LLM-driven graph extraction ───────────────────────────────────────────────

/// Mock [`LlmGraphExtractor`] for unit tests. Returns a canned
/// `LlmGraphExtraction` (or a forced error) and counts invocations.
struct MockGraphExtractor {
    name: &'static str,
    model_label: String,
    response: Result<LlmGraphExtraction, &'static str>,
    calls: AtomicUsize,
}

impl MockGraphExtractor {
    fn with_response(response: LlmGraphExtraction) -> Self {
        Self {
            name: "mock:llm-graph",
            model_label: "mock:llm-graph-v1".into(),
            response: Ok(response),
            calls: AtomicUsize::new(0),
        }
    }

    fn with_error(message: &'static str) -> Self {
        Self {
            name: "mock:llm-graph",
            model_label: "mock:llm-graph-v1".into(),
            response: Err(message),
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmGraphExtractor for MockGraphExtractor {
    fn name(&self) -> &str {
        self.name
    }
    fn model_label(&self) -> &str {
        &self.model_label
    }
    async fn extract_graph(
        &self,
        _content: &str,
        _title: &str,
    ) -> anyhow::Result<LlmGraphExtraction> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.response {
            Ok(out) => Ok(out.clone()),
            Err(msg) => Err(anyhow::anyhow!(*msg)),
        }
    }
}

fn ingest_request(namespace: &str, title: &str, content: &str) -> MemoryIngestionRequest {
    MemoryIngestionRequest {
        document: NamespaceDocumentInput {
            namespace: namespace.to_string(),
            key: format!("{namespace}-{title}"),
            title: title.to_string(),
            content: content.to_string(),
            source_type: "test".to_string(),
            priority: "medium".to_string(),
            tags: Vec::new(),
            metadata: json!({}),
            category: "core".to_string(),
            session_id: None,
            document_id: None,
        },
        config: MemoryIngestionConfig::default(),
    }
}

#[tokio::test]
async fn heuristic_only_runs_when_no_extractor_configured() {
    let tmp = TempDir::new().unwrap();
    let memory = UnifiedMemory::new(tmp.path(), Arc::new(NoopEmbedding), None).unwrap();
    let result = memory
        .ingest_document(ingest_request(
            "skill-test-heuristic",
            "Heuristic only doc",
            "Owner: Alice\nName: refactor api\nProject name: OpenHuman\n",
        ))
        .await
        .unwrap();
    assert_eq!(
        result.extraction_backend, "heuristic",
        "expected backend label 'heuristic' when no extractor is wired"
    );
    assert!(
        result.model_name.starts_with("heuristic"),
        "model_name should remain the heuristic-only literal, got: {}",
        result.model_name
    );
}

#[tokio::test]
async fn llm_extraction_merges_into_namespace_graph() {
    let tmp = TempDir::new().unwrap();
    let memory = UnifiedMemory::new(tmp.path(), Arc::new(NoopEmbedding), None).unwrap();
    let extractor = Arc::new(MockGraphExtractor::with_response(LlmGraphExtraction {
        entities: vec![],
        relations: vec![
            LlmRelationSpec {
                subject: "Carol".into(),
                subject_type: "PERSON".into(),
                predicate: "OWNS".into(),
                object: "memory_tree refactor".into(),
                object_type: "WORK_ITEM".into(),
                confidence: 0.93,
            },
            LlmRelationSpec {
                subject: "Carol".into(),
                subject_type: "PERSON".into(),
                predicate: "BREATHES".into(),
                object: "air".into(),
                object_type: "TOPIC".into(),
                confidence: 0.99,
            },
        ],
    }));
    let mut request = ingest_request(
        "skill-test-llm",
        "LLM-augmented doc",
        "Topic: capability review.\n\nDiscussion of upcoming work.",
    );
    request.config.graph_extraction = GraphExtractionMode::Llm;
    let result = memory
        .ingest_document_with_extractor(request, Some(extractor.clone()))
        .await
        .unwrap();
    assert!(
        extractor.call_count() >= 1,
        "expected extractor to be invoked at least once"
    );
    assert_eq!(
        result.model_name, "mock:llm-graph-v1",
        "result.model_name should reflect the extractor's model_label, got: {}",
        result.model_name
    );
    assert!(
        result.extraction_backend == "llm" || result.extraction_backend == "llm+heuristic",
        "expected backend label to mention llm, got: {}",
        result.extraction_backend
    );
    assert!(
        result
            .relations
            .iter()
            .any(|r| r.subject == "CAROL"
                && r.predicate == "OWNS"
                && r.object.contains("MEMORY_TREE REFACTOR")),
        "expected the LLM-emitted (Carol, OWNS, memory_tree refactor) triple to land — got {:?}",
        result.relations
    );
    assert!(
        !result
            .relations
            .iter()
            .any(|r| r.predicate.contains("BREATHE")),
        "invalid predicate from LLM should be dropped by the accumulator's rule filter"
    );

    let rows = memory
        .graph_query_namespace("skill-test-llm", Some("CAROL"), Some("OWNS"))
        .await
        .unwrap();
    assert!(
        !rows.is_empty(),
        "expected graph_query_namespace to return the LLM-extracted (Carol, OWNS, …) row"
    );
}

#[tokio::test]
async fn llm_failure_falls_back_to_heuristic() {
    let tmp = TempDir::new().unwrap();
    let memory = UnifiedMemory::new(tmp.path(), Arc::new(NoopEmbedding), None).unwrap();
    let extractor = Arc::new(MockGraphExtractor::with_error("provider unreachable"));
    let mut request = ingest_request(
        "skill-test-fallback",
        "Fallback doc",
        "Owner: Dave\nName: write report\nProject name: OpenHuman\n",
    );
    request.config.graph_extraction = GraphExtractionMode::Llm;
    let result = memory
        .ingest_document_with_extractor(request, Some(extractor.clone()))
        .await
        .unwrap();
    assert_eq!(
        extractor.call_count(),
        1,
        "extractor should be invoked exactly once on failure"
    );
    assert_eq!(
        result.extraction_backend, "heuristic (llm fallback)",
        "expected heuristic-fallback backend label after LLM failure, got: {}",
        result.extraction_backend
    );
    assert_eq!(
        result.model_name, "mock:llm-graph-v1",
        "model label is preserved through fallback so observers can see what was tried"
    );
    assert!(
        result
            .relations
            .iter()
            .any(|r| r.subject == "DAVE" && r.predicate == "OWNS"),
        "heuristic OWNS triple should survive even when LLM fails"
    );
}

#[tokio::test]
async fn heuristic_mode_skips_extractor_entirely() {
    let tmp = TempDir::new().unwrap();
    let memory = UnifiedMemory::new(tmp.path(), Arc::new(NoopEmbedding), None).unwrap();
    let extractor = Arc::new(MockGraphExtractor::with_response(LlmGraphExtraction {
        entities: vec![],
        relations: vec![LlmRelationSpec {
            subject: "Eve".into(),
            subject_type: "PERSON".into(),
            predicate: "OWNS".into(),
            object: "exfiltration".into(),
            object_type: "WORK_ITEM".into(),
            confidence: 0.99,
        }],
    }));
    let mut request = ingest_request(
        "skill-test-skip",
        "Heuristic mode",
        "Owner: Frank\nName: legitimate task\nProject name: OpenHuman\n",
    );
    request.config.graph_extraction = GraphExtractionMode::Heuristic;
    let result = memory
        .ingest_document_with_extractor(request, Some(extractor.clone()))
        .await
        .unwrap();
    assert_eq!(
        extractor.call_count(),
        0,
        "extractor must NOT be called in Heuristic mode"
    );
    assert_eq!(result.extraction_backend, "heuristic");
    assert!(
        !result
            .relations
            .iter()
            .any(|r| r.subject == "EVE" || r.object.contains("EXFILTRATION")),
        "Heuristic mode must not surface any LLM-suggested entities/relations"
    );
}

#[tokio::test]
async fn auto_mode_without_extractor_runs_heuristic_silently() {
    let tmp = TempDir::new().unwrap();
    let memory = UnifiedMemory::new(tmp.path(), Arc::new(NoopEmbedding), None).unwrap();
    let mut request = ingest_request(
        "skill-test-auto",
        "Auto fallback",
        "Owner: Heidi\nName: review patch\nProject name: OpenHuman\n",
    );
    request.config.graph_extraction = GraphExtractionMode::Auto;
    let result = memory.ingest_document(request).await.unwrap();
    assert_eq!(
        result.extraction_backend, "heuristic",
        "Auto with no extractor should silently degrade to heuristic"
    );
    assert!(
        result
            .relations
            .iter()
            .any(|r| r.subject == "HEIDI" && r.predicate == "OWNS"),
        "heuristic OWNS triple should still land"
    );
}
