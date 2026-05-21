//! Public and private types for the memory ingestion pipeline.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::openhuman::config::GraphExtractionMode;
use crate::openhuman::memory::store::types::NamespaceDocumentInput;

/// Default extraction backend label reported in ingestion metadata.
pub const DEFAULT_MEMORY_EXTRACTION_MODEL: &str = "heuristic-only";
/// Backend identifier used in `MemoryIngestionResult.extraction_backend`.
/// Stable wire strings — surfaced in logs, UI activity rows, and the
/// `ingestion.backend` field in document metadata.
pub mod extraction_backend {
    /// Only the heuristic regex extractor ran (LLM disabled or unavailable).
    pub const HEURISTIC: &str = "heuristic";
    /// Only the LLM extractor produced data (heuristic ran for structural
    /// metadata but contributed no entities/relations on top).
    pub const LLM: &str = "llm";
    /// Both extractors contributed entities/relations. The accumulator
    /// alias-resolution + predicate-rule filter has already merged them.
    pub const LLM_PLUS_HEURISTIC: &str = "llm+heuristic";
    /// LLM was attempted but failed; heuristic-only output was used.
    pub const HEURISTIC_FALLBACK: &str = "heuristic (llm fallback)";
}
/// Default number of tokens per text chunk during ingestion.
pub(super) const DEFAULT_CHUNK_TOKENS: usize = 225;

/// Granularity of extraction for heuristic parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExtractionMode {
    /// Extract from each individual sentence (higher precision).
    #[default]
    Sentence,
    /// Extract from the entire chunk at once (faster, better for context).
    Chunk,
}

impl ExtractionMode {
    /// Returns the string representation of the extraction mode.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Sentence => "sentence",
            Self::Chunk => "chunk",
        }
    }
}

/// Configuration for the memory ingestion process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryIngestionConfig {
    /// Extraction backend label recorded in metadata/results.
    pub model_name: String,
    /// The granularity of heuristic extraction.
    #[serde(default)]
    pub extraction_mode: ExtractionMode,
    /// Minimum confidence threshold for entity extraction (0.0 to 1.0).
    #[serde(default = "default_entity_threshold")]
    pub entity_threshold: f32,
    /// Minimum confidence threshold for relation extraction (0.0 to 1.0).
    #[serde(default = "default_relation_threshold")]
    pub relation_threshold: f32,
    /// Threshold for adjacency-based heuristics.
    #[serde(default = "default_adjacency_threshold")]
    pub adjacency_threshold: f32,
    /// Reserved batch-size knob kept for config compatibility.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// Strategy for populating the namespace knowledge graph. Mirrors
    /// the `memory.graph_extraction` config knob — exposed on
    /// [`MemoryIngestionConfig`] so callers that build a one-off
    /// request (e.g. RPC handlers) can override it without touching
    /// the global config.
    #[serde(default)]
    pub graph_extraction: GraphExtractionMode,
}

fn default_entity_threshold() -> f32 {
    0.45
}

fn default_relation_threshold() -> f32 {
    0.30
}

fn default_adjacency_threshold() -> f32 {
    0.50
}

fn default_batch_size() -> usize {
    16
}

impl Default for MemoryIngestionConfig {
    fn default() -> Self {
        Self {
            model_name: DEFAULT_MEMORY_EXTRACTION_MODEL.to_string(),
            extraction_mode: ExtractionMode::Sentence,
            entity_threshold: default_entity_threshold(),
            relation_threshold: default_relation_threshold(),
            adjacency_threshold: default_adjacency_threshold(),
            batch_size: default_batch_size(),
            graph_extraction: GraphExtractionMode::default(),
        }
    }
}

/// A request to ingest a single document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryIngestionRequest {
    /// The document input to process.
    pub document: NamespaceDocumentInput,
    /// Ingestion configuration.
    #[serde(default)]
    pub config: MemoryIngestionConfig,
}

/// An entity identified during the ingestion process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedEntity {
    /// Normalized name of the entity (all-caps).
    pub name: String,
    /// Classification (e.g., PERSON, ORGANIZATION).
    pub entity_type: String,
    /// Known aliases for this entity.
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// A relation identified during the ingestion process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedRelation {
    /// Name of the subject entity.
    pub subject: String,
    /// Classification of the subject.
    pub subject_type: String,
    /// Relationship type (e.g., OWNS, WORKS_ON).
    pub predicate: String,
    /// Name of the object entity.
    pub object: String,
    /// Classification of the object.
    pub object_type: String,
    /// Extraction confidence (0.0 to 1.0).
    pub confidence: f32,
    /// Number of distinct occurrences of this relation.
    pub evidence_count: u32,
    /// IDs of the chunks where this relation was found.
    pub chunk_ids: Vec<String>,
    /// Sequential order index for reconstruction.
    pub order_index: Option<i64>,
    /// Additional metadata about the extraction.
    pub metadata: Value,
}

/// The comprehensive result of an ingestion operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryIngestionResult {
    /// ID of the document that was ingested.
    pub document_id: String,
    /// Namespace containing the document.
    pub namespace: String,
    /// Extraction backend label recorded for the ingestion run.
    /// This is the model identifier (e.g. `"openai:gpt-5.4-mini"`,
    /// `"ollama:gemma4:e4b"`) when the LLM extractor ran; falls back
    /// to [`DEFAULT_MEMORY_EXTRACTION_MODEL`] (`heuristic-only`) when
    /// only the regex path produced output.
    pub model_name: String,
    /// Which extractor(s) contributed to the final entity / relation
    /// lists. One of the [`extraction_backend`] constants
    /// (`heuristic`, `llm`, `llm+heuristic`, `heuristic (llm fallback)`).
    /// Surfaced to the UI / activity log so users can see whether their
    /// configured `memory_provider` is actually being hit.
    #[serde(default)]
    pub extraction_backend: String,
    /// Mode used for extraction (sentence|chunk granularity for the
    /// heuristic path).
    pub extraction_mode: String,
    /// Total number of chunks processed.
    pub chunk_count: usize,
    /// Total number of distinct entities found.
    pub entity_count: usize,
    /// Total number of distinct relations found.
    pub relation_count: usize,
    /// Number of identified user preferences.
    pub preference_count: usize,
    /// Number of identified decisions.
    pub decision_count: usize,
    /// Auto-generated tags for the document.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Complete list of identified entities.
    #[serde(default)]
    pub entities: Vec<ExtractedEntity>,
    /// Complete list of identified relations.
    #[serde(default)]
    pub relations: Vec<ExtractedRelation>,
}

/// Intermediate representation of an entity before normalization and alias resolution.
#[derive(Debug, Clone)]
pub(super) struct RawEntity {
    pub(super) name: String,
    pub(super) entity_type: String,
    pub(super) confidence: f32,
}

/// Intermediate representation of a relationship before aggregation.
#[derive(Debug, Clone)]
pub(super) struct RawRelation {
    pub(super) subject: String,
    pub(super) subject_type: String,
    pub(super) predicate: String,
    pub(super) object: String,
    pub(super) object_type: String,
    pub(super) confidence: f32,
    /// Indices of the chunks where this relation was found.
    pub(super) chunk_indexes: BTreeSet<usize>,
    /// Global sequential index for ordering within the document.
    pub(super) order_index: i64,
    /// JSON metadata for the relation.
    pub(super) metadata: Map<String, Value>,
}

/// A single unit of text (sentence or chunk) passed to the extractor.
#[derive(Debug, Clone)]
pub(super) struct ExtractionUnit {
    pub(super) text: String,
    pub(super) chunk_index: usize,
    pub(super) order_index: i64,
}

/// Accumulates extraction results across multiple chunks or units.
///
/// Handles entity and relation deduplication, alias tracking, and
/// basic document understanding (e.g., identifying the primary subject).
#[derive(Debug, Default)]
pub(super) struct ExtractionAccumulator {
    /// Mapping of normalized entity name to its highest-confidence raw extraction.
    pub(super) entities: HashMap<String, RawEntity>,
    /// Collected relations before final canonicalization.
    pub(super) relations: Vec<RawRelation>,
    /// Tags identified during processing.
    pub(super) tags: BTreeSet<String>,
    /// Decisions identified during processing.
    pub(super) decisions: BTreeSet<String>,
    /// User preferences identified during processing.
    pub(super) preferences: BTreeSet<String>,
    /// Inferred document kind (e.g., "profile").
    pub(super) doc_kind: Option<String>,
    /// The document's inferred primary subject.
    pub(super) primary_subject: Option<String>,
    /// Sanitized document title.
    pub(super) document_title: Option<String>,
    /// The subject of the current markdown section.
    pub(super) current_subject: Option<String>,
    /// Current sender if processing a message/thread.
    pub(super) current_sender: Option<String>,
    /// Mapping of names to their canonicalized full name.
    pub(super) known_people: HashMap<String, String>,
}

/// The result of the parsing stage of ingestion.
#[derive(Debug)]
pub(super) struct ParsedIngestion {
    pub(super) tags: Vec<String>,
    pub(super) metadata: Value,
    pub(super) entities: Vec<ExtractedEntity>,
    pub(super) relations: Vec<ExtractedRelation>,
    pub(super) chunk_count: usize,
    pub(super) preference_count: usize,
    pub(super) decision_count: usize,
    /// Which extractor(s) contributed. See
    /// [`extraction_backend`](crate::openhuman::memory::ingestion::types::extraction_backend).
    pub(super) extraction_backend: String,
    /// Resolved model identifier when the LLM extractor ran; `None`
    /// when only the heuristic path produced output.
    pub(super) model_label: Option<String>,
}
