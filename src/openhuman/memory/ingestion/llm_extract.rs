//! LLM-driven entity and relation extraction for the namespace knowledge graph.
//!
//! Mirrors the shape of [`crate::openhuman::memory::tree::score::extract::llm`]
//! but emits the `(subject, predicate, object)` triples the namespace graph
//! reads via `graph_query_namespace` / `query_namespace`, instead of NER spans
//! consumed by the memory-tree summariser.
//!
//! ## Soft-fallback contract
//!
//! `extract_graph` is allowed to fail with `Err`. The ingestion pipeline
//! ([`super::parse::parse_document`]) catches every `Err` (provider unreachable,
//! malformed JSON, schema mismatch, …), logs a `[memory:ingestion]` warning,
//! and falls back to the heuristic-only path so document ingest stays
//! write-through. Implementations therefore do not need to invent their own
//! fallback envelope.
//!
//! ## Why a separate trait
//!
//! [`crate::openhuman::memory::tree::chat::ChatProvider`] is the right
//! abstraction for "send a prompt, get a JSON string back" — the LLM graph
//! extractor builds on it but exposes a higher-level surface
//! (`extract_graph(content, title) -> LlmGraphExtraction`) so the
//! ingestion path doesn't have to know about prompts or JSON envelopes.
//! Tests can mock [`LlmGraphExtractor`] directly without standing up a
//! full chat-provider stack.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::openhuman::memory::tree::chat::{ChatPrompt, ChatProvider};

/// Output of a single [`LlmGraphExtractor::extract_graph`] call.
///
/// Entities and relations are flat — the ingestion accumulator
/// ([`super::types::ExtractionAccumulator`]) does the alias resolution,
/// predicate-rule validation, and final aggregation, so the extractor's
/// job is just to surface candidates with confidences.
#[derive(Debug, Clone, Default)]
pub struct LlmGraphExtraction {
    /// Standalone entity mentions the model wants to register in the graph.
    /// Most callers can leave this empty and rely on entities being
    /// implicitly created from the subject/object of each relation, but
    /// emitting them explicitly lets the model surface entities that
    /// don't appear in any `(s, p, o)` triple it found.
    pub entities: Vec<LlmEntitySpec>,
    /// Subject/predicate/object triples for the namespace graph.
    pub relations: Vec<LlmRelationSpec>,
}

/// A standalone entity candidate emitted by the LLM extractor.
#[derive(Debug, Clone)]
pub struct LlmEntitySpec {
    /// Entity name (free-form; the accumulator will sanitise + uppercase).
    pub name: String,
    /// Entity type — one of the validation rule's accepted types
    /// (`PERSON`, `ORGANIZATION`, `PROJECT`, `PRODUCT`, `TOOL`, `TOPIC`,
    /// `WORK_ITEM`, `PLACE`, `LOCATION`, `ROOM`, `DATE`, `MODE`). Anything
    /// else still flows through the accumulator's `add_entity`, but the
    /// downstream predicate-rule check will drop relations whose endpoint
    /// types don't fit. Uppercased before validation.
    pub entity_type: String,
    /// Extractor confidence in [0.0, 1.0]. The pipeline's entity_threshold
    /// filters before final emission.
    pub confidence: f32,
}

/// A (subject, predicate, object) candidate emitted by the LLM extractor.
#[derive(Debug, Clone)]
pub struct LlmRelationSpec {
    /// Subject entity name.
    pub subject: String,
    /// Subject entity type (uppercase). See [`LlmEntitySpec::entity_type`].
    pub subject_type: String,
    /// Predicate (free-form). The accumulator normalises via
    /// `UnifiedMemory::normalize_graph_predicate` and looks up the
    /// validation rule in [`super::rules::relation_rule`]. Unknown
    /// predicates are dropped.
    pub predicate: String,
    /// Object entity name.
    pub object: String,
    /// Object entity type (uppercase).
    pub object_type: String,
    /// Extractor confidence in [0.0, 1.0]. The pipeline's relation_threshold
    /// filters before final emission.
    pub confidence: f32,
}

/// Pluggable LLM-driven graph extractor used by
/// [`super::parse::parse_document`].
///
/// Implementations should:
/// - Build a structured-JSON prompt asking for `(s, p, o)` triples.
/// - Send it to the user's configured `memory_provider` workload.
/// - Parse the response into [`LlmGraphExtraction`].
/// - Return `Err` on any failure; the pipeline soft-falls back to the
///   heuristic extractor.
#[async_trait]
pub trait LlmGraphExtractor: Send + Sync {
    /// Stable, grep-friendly name for logs (e.g. `"openai:gpt-5.4-mini"`).
    fn name(&self) -> &str;

    /// Identifier for the underlying model — surfaced through
    /// [`super::types::MemoryIngestionResult::model_name`] so the UI / activity
    /// log can show "extracted via gemma4:e4b" instead of the cosmetic
    /// `heuristic-only` label.
    fn model_label(&self) -> &str;

    /// Extract subject/predicate/object triples + standalone entities from
    /// `content`. `title` is provided so the prompt can disambiguate the
    /// document (e.g. headers in markdown / subject lines in email).
    async fn extract_graph(
        &self,
        content: &str,
        title: &str,
    ) -> anyhow::Result<LlmGraphExtraction>;
}

/// Default [`LlmGraphExtractor`] backed by a memory-tree
/// [`ChatProvider`]. Builds the system prompt, sends a single chat call
/// with `temperature = 0.0`, and parses the response.
pub struct ChatBackedLlmGraphExtractor {
    provider: Arc<dyn ChatProvider>,
    model_label: String,
}

impl ChatBackedLlmGraphExtractor {
    /// Wrap a chat provider. `model_label` is a free-form display string
    /// surfaced through [`LlmGraphExtractor::model_label`] for reporting
    /// (e.g. `"openai:gpt-5.4-mini"` or `"ollama:gemma4:e4b"`).
    pub fn new(provider: Arc<dyn ChatProvider>, model_label: impl Into<String>) -> Self {
        Self {
            provider,
            model_label: model_label.into(),
        }
    }
}

#[async_trait]
impl LlmGraphExtractor for ChatBackedLlmGraphExtractor {
    fn name(&self) -> &str {
        self.provider.name()
    }

    fn model_label(&self) -> &str {
        &self.model_label
    }

    async fn extract_graph(
        &self,
        content: &str,
        title: &str,
    ) -> anyhow::Result<LlmGraphExtraction> {
        let prompt = ChatPrompt {
            system: build_system_prompt(),
            user: build_user_prompt(content, title),
            temperature: 0.0,
            kind: "memory::ingestion::graph",
        };
        log::debug!(
            "[memory:ingestion::llm] extract_graph provider={} model={} \
             title_chars={} content_chars={}",
            self.provider.name(),
            self.model_label,
            title.chars().count(),
            content.chars().count(),
        );
        let raw = self.provider.chat_for_json(&prompt).await?;
        log::debug!(
            "[memory:ingestion::llm] response chars={} provider={}",
            raw.len(),
            self.provider.name()
        );
        parse_llm_output(&raw)
    }
}

// ── Prompt ───────────────────────────────────────────────────────────────

fn build_system_prompt() -> String {
    // Predicate set mirrors the accepted rules in
    // `memory::ingestion::rules::relation_rule`. Entity-type set mirrors
    // the constants there too. Keeping the model on the strict
    // vocabulary keeps the predicate-rule filter from dropping
    // otherwise-good triples on a synonym ("ASSIGNED_TO" instead of
    // "OWNS", say).
    "You are an entity-and-relation extractor for a personal knowledge graph. \
Return JSON only — no prose, no markdown, no commentary. Extract every \
subject/predicate/object triple you can find in the text. Use only the \
predicates and entity types from the controlled vocabularies below; if \
something doesn't fit, omit it rather than guessing.\n\n\
Schema:\n\
{\n\
  \"entities\": [\n\
    { \"name\": \"<canonical name>\", \"type\": \"<ENTITY_TYPE>\", \"confidence\": 0.0 }\n\
  ],\n\
  \"relations\": [\n\
    { \"subject\": \"<name>\", \"subject_type\": \"<ENTITY_TYPE>\",\n\
      \"predicate\": \"<PREDICATE>\",\n\
      \"object\": \"<name>\", \"object_type\": \"<ENTITY_TYPE>\",\n\
      \"confidence\": 0.0 }\n\
  ]\n\
}\n\n\
Entity types (use exactly one per entity, uppercase):\n\
  PERSON       named human (\"Alice\", \"Steven Enamakel\")\n\
  ORGANIZATION company / team (\"Anthropic\", \"TinyHumans\")\n\
  PROJECT      named effort / initiative (\"OpenHuman\", \"core rewrite\")\n\
  PRODUCT      commercial offering (\"Claude Code\", \"Slack\")\n\
  TOOL         framework / library / tool (\"Rust\", \"JSON-RPC\", \"OAuth\")\n\
  TOPIC        abstract theme / concept (\"memory tree\", \"rate limiting\")\n\
  WORK_ITEM    task / ticket / deliverable (\"PR #934\", \"OH-42\")\n\
  PLACE        broad geography (\"London\", \"SF\")\n\
  LOCATION     specific place (\"the SF office\", \"room 4B\")\n\
  ROOM         a room (\"kitchen\", \"north hallway\")\n\
  DATE         temporal expression (\"Friday\", \"Q2 2026\", \"EOD tomorrow\")\n\
  MODE         configuration / preference choice (\"chunk\", \"sentence\")\n\n\
Predicates (use exactly one per relation, uppercase snake_case):\n\
  OWNS                 PERSON owns WORK_ITEM / PROJECT / TOPIC / PRODUCT / TOOL\n\
  WORKS_ON             PERSON works on PROJECT (mapped to OWNS)\n\
  WORKS_FOR            PERSON works for ORGANIZATION / PROJECT / PRODUCT\n\
  USES                 PROJECT / TOPIC / WORK_ITEM uses TOOL / TOPIC / PRODUCT\n\
  ADOPTS               same shape as USES\n\
  KEEPS                same shape as USES\n\
  DEPENDS_ON           PROJECT / WORK_ITEM depends on TOOL / TOPIC / PRODUCT\n\
  PREFERS              PERSON prefers TOPIC / WORK_ITEM / MODE / PRODUCT / TOOL\n\
  HAS_DEADLINE         PROJECT / WORK_ITEM / TOPIC has deadline DATE\n\
  DUE_ON               same shape as HAS_DEADLINE\n\
  COMMUNICATES_WITH    PERSON communicates with PERSON\n\
  INVESTIGATES         PERSON investigates PROJECT / TOPIC / WORK_ITEM\n\
  EVALUATES            same shape as INVESTIGATES\n\
  REVIEWS              PERSON reviews WORK_ITEM (mapped to OWNS)\n\
  AVOIDS               PROJECT / TOPIC avoids TOOL / TOPIC\n\
  NORTH_OF / SOUTH_OF / EAST_OF / WEST_OF   ROOM near another ROOM\n\n\
Rules:\n\
- Only emit entities and predicates from the lists above. Omit anything else.\n\
- Subject and object types must match the predicate's column above; \
otherwise omit the triple.\n\
- Confidences are 0.0–1.0. Use 0.9+ for explicit textual evidence, \
0.7+ for clear inference, lower for tentative.\n\
- Do not invent entities not present in the text. Use the exact surface form \
the text uses (the consumer will canonicalise / uppercase).\n\
- Always emit both top-level fields (entities, relations), even when empty.\n\
\n\
Example:\n\
Input: \"From: Alice <a@x.io>\\nTo: Bob <b@x.io>\\nSubject: Q2 launch\\n\\n\
Bob owns the auth migration; due 2026-06-30. The team uses OAuth.\"\n\
Output: {\"entities\":[{\"name\":\"Alice\",\"type\":\"PERSON\",\"confidence\":0.95},\
{\"name\":\"Bob\",\"type\":\"PERSON\",\"confidence\":0.95},\
{\"name\":\"auth migration\",\"type\":\"WORK_ITEM\",\"confidence\":0.9},\
{\"name\":\"2026-06-30\",\"type\":\"DATE\",\"confidence\":0.95},\
{\"name\":\"OAuth\",\"type\":\"TOOL\",\"confidence\":0.9}],\
\"relations\":[\
{\"subject\":\"Alice\",\"subject_type\":\"PERSON\",\"predicate\":\"COMMUNICATES_WITH\",\
\"object\":\"Bob\",\"object_type\":\"PERSON\",\"confidence\":0.95},\
{\"subject\":\"Bob\",\"subject_type\":\"PERSON\",\"predicate\":\"OWNS\",\
\"object\":\"auth migration\",\"object_type\":\"WORK_ITEM\",\"confidence\":0.95},\
{\"subject\":\"auth migration\",\"subject_type\":\"WORK_ITEM\",\"predicate\":\"HAS_DEADLINE\",\
\"object\":\"2026-06-30\",\"object_type\":\"DATE\",\"confidence\":0.95},\
{\"subject\":\"auth migration\",\"subject_type\":\"WORK_ITEM\",\"predicate\":\"USES\",\
\"object\":\"OAuth\",\"object_type\":\"TOOL\",\"confidence\":0.9}]}\n"
        .to_string()
}

fn build_user_prompt(content: &str, title: &str) -> String {
    let trimmed_title = title.trim();
    if trimmed_title.is_empty() {
        format!("Text:\n{content}\n\nReturn JSON only.")
    } else {
        format!("Title: {trimmed_title}\n\nText:\n{content}\n\nReturn JSON only.")
    }
}

// ── Parsing ──────────────────────────────────────────────────────────────

/// Parse a chat response into a [`LlmGraphExtraction`].
///
/// Tolerates pre/post junk around the `{...}` envelope so models that wrap
/// JSON in `<json>...</json>` or markdown fences still parse. Returns
/// `Err` only when no valid JSON object can be extracted — the caller's
/// soft-fallback then kicks in.
pub(super) fn parse_llm_output(raw: &str) -> anyhow::Result<LlmGraphExtraction> {
    let body = extract_json_object(raw).unwrap_or(raw);
    let parsed: LlmRawOutput = serde_json::from_str(body).map_err(|e| {
        anyhow::anyhow!(
            "namespace-graph LLM returned non-JSON (or wrong shape): {e}; content was: {}",
            truncate_for_log(raw, 400),
        )
    })?;

    let entities = parsed
        .entities
        .into_iter()
        .filter_map(|raw| {
            let name = raw.name.trim();
            let etype = raw.entity_type.trim();
            if name.is_empty() || etype.is_empty() {
                return None;
            }
            Some(LlmEntitySpec {
                name: name.to_string(),
                entity_type: etype.to_uppercase(),
                confidence: raw.confidence.unwrap_or(0.80).clamp(0.0, 1.0),
            })
        })
        .collect();

    let relations = parsed
        .relations
        .into_iter()
        .filter_map(|raw| {
            let subject = raw.subject.trim();
            let predicate = raw.predicate.trim();
            let object = raw.object.trim();
            if subject.is_empty() || predicate.is_empty() || object.is_empty() {
                return None;
            }
            // Default to TOPIC when the model omits a type — the
            // accumulator's predicate-rule check will still drop the
            // triple if the predicate column requires something else.
            let s_type = raw.subject_type.trim();
            let o_type = raw.object_type.trim();
            Some(LlmRelationSpec {
                subject: subject.to_string(),
                subject_type: if s_type.is_empty() {
                    "TOPIC".to_string()
                } else {
                    s_type.to_uppercase()
                },
                predicate: predicate.to_uppercase(),
                object: object.to_string(),
                object_type: if o_type.is_empty() {
                    "TOPIC".to_string()
                } else {
                    o_type.to_uppercase()
                },
                confidence: raw.confidence.unwrap_or(0.80).clamp(0.0, 1.0),
            })
        })
        .collect();

    Ok(LlmGraphExtraction {
        entities,
        relations,
    })
}

fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

fn truncate_for_log(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}…")
}

#[derive(Debug, Deserialize)]
struct LlmRawOutput {
    #[serde(default)]
    entities: Vec<LlmRawEntity>,
    #[serde(default)]
    relations: Vec<LlmRawRelation>,
}

#[derive(Debug, Deserialize)]
struct LlmRawEntity {
    #[serde(default, alias = "entity", alias = "label")]
    name: String,
    #[serde(default, alias = "type", alias = "entity_type", alias = "entityType")]
    entity_type: String,
    #[serde(default)]
    confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct LlmRawRelation {
    #[serde(default, alias = "head")]
    subject: String,
    #[serde(default, alias = "subjectType", alias = "head_type", alias = "headType")]
    subject_type: String,
    #[serde(default, alias = "relation", alias = "rel")]
    predicate: String,
    #[serde(default, alias = "tail")]
    object: String,
    #[serde(default, alias = "objectType", alias = "tail_type", alias = "tailType")]
    object_type: String,
    #[serde(default)]
    confidence: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_llm_output_accepts_canonical_envelope() {
        let raw = r#"{
            "entities": [
                {"name": "Alice", "type": "PERSON", "confidence": 0.9},
                {"name": "OpenHuman", "type": "PROJECT", "confidence": 0.85}
            ],
            "relations": [
                {"subject": "Alice", "subject_type": "PERSON",
                 "predicate": "WORKS_FOR",
                 "object": "OpenHuman", "object_type": "PROJECT",
                 "confidence": 0.9}
            ]
        }"#;
        let out = parse_llm_output(raw).unwrap();
        assert_eq!(out.entities.len(), 2);
        assert_eq!(out.entities[0].name, "Alice");
        assert_eq!(out.entities[0].entity_type, "PERSON");
        assert!((out.entities[0].confidence - 0.9).abs() < 1e-4);
        assert_eq!(out.relations.len(), 1);
        assert_eq!(out.relations[0].subject, "Alice");
        assert_eq!(out.relations[0].predicate, "WORKS_FOR");
    }

    #[test]
    fn parse_llm_output_tolerates_alias_field_names() {
        let raw = r#"{
            "entities": [{"label": "Bob", "entity_type": "person"}],
            "relations": [{
                "head": "Bob", "head_type": "person",
                "relation": "owns",
                "tail": "PR-42", "tail_type": "work_item",
                "confidence": 1.5
            }]
        }"#;
        let out = parse_llm_output(raw).unwrap();
        assert_eq!(out.entities[0].name, "Bob");
        // Uppercased.
        assert_eq!(out.entities[0].entity_type, "PERSON");
        // Default confidence applied (missing).
        assert!((out.entities[0].confidence - 0.80).abs() < 1e-4);
        assert_eq!(out.relations[0].subject, "Bob");
        assert_eq!(out.relations[0].predicate, "OWNS");
        // Confidence clamped to 1.0.
        assert!((out.relations[0].confidence - 1.0).abs() < 1e-4);
    }

    #[test]
    fn parse_llm_output_strips_markdown_fence() {
        let raw = "```json\n{\n  \"entities\": [],\n  \"relations\": []\n}\n```";
        let out = parse_llm_output(raw).unwrap();
        assert!(out.entities.is_empty());
        assert!(out.relations.is_empty());
    }

    #[test]
    fn parse_llm_output_errors_on_bare_text() {
        let err = parse_llm_output("sorry I can't help with that")
            .err()
            .expect("expected an error for non-JSON output");
        assert!(format!("{err:#}").contains("non-JSON"));
    }

    #[test]
    fn parse_llm_output_drops_empty_entries() {
        let raw = r#"{
            "entities": [
                {"name": "", "type": "PERSON"},
                {"name": "Bob", "type": ""}
            ],
            "relations": [
                {"subject": "", "predicate": "OWNS", "object": "X"},
                {"subject": "S", "predicate": "", "object": "O"},
                {"subject": "S", "predicate": "OWNS", "object": ""},
                {"subject": "Alice", "predicate": "OWNS", "object": "Repo"}
            ]
        }"#;
        let out = parse_llm_output(raw).unwrap();
        assert!(out.entities.is_empty(), "both entries had empty fields");
        assert_eq!(
            out.relations.len(),
            1,
            "only the fully populated triple survives"
        );
        assert_eq!(out.relations[0].subject, "Alice");
        // Missing subject/object types default to TOPIC.
        assert_eq!(out.relations[0].subject_type, "TOPIC");
        assert_eq!(out.relations[0].object_type, "TOPIC");
    }

    #[test]
    fn extract_json_object_handles_pre_and_post_junk() {
        let s = "Sure! Here's the JSON:\n{\"entities\":[],\"relations\":[]}\nLet me know if you need anything else.";
        let body = extract_json_object(s).unwrap();
        assert!(body.starts_with('{') && body.ends_with('}'));
    }

    #[test]
    fn build_user_prompt_omits_blank_title() {
        let with_title = build_user_prompt("hello", "Subject");
        assert!(with_title.starts_with("Title: Subject"));
        let no_title = build_user_prompt("hello", "   ");
        assert!(no_title.starts_with("Text:"));
    }
}
