//! Translate a parsed classifier decision into side effects.
//!
//! The four actions:
//!
//! - **`drop`** — log only, publish `TriggerEvaluated`.
//! - **`acknowledge`** — log, publish `TriggerEvaluated`, and write
//!   a deterministic memo into the memory tree so the agent can recall
//!   the triggered event later. Memory-write is best-effort: a failure
//!   logs a warning and does not fail the triage pipeline.
//! - **`react`** — dispatch the `trigger_reactor` sub-agent via
//!   [`run_subagent`], publish `TriggerEvaluated` + `TriggerEscalated`.
//! - **`escalate`** — dispatch the `orchestrator` sub-agent, same
//!   events.
//!
//! `react`/`escalate` build a full [`Agent`] from config so they have
//! a real provider, tool registry, and memory backing — the same
//! construction path `agent_chat` uses. A [`ParentExecutionContext`] is
//! installed on the task-local so [`run_subagent`] can inherit the
//! provider and tools.

use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{anyhow, Context};

use crate::openhuman::agent::harness::definition::AgentDefinitionRegistry;
use crate::openhuman::agent::harness::fork_context::{with_parent_context, ParentExecutionContext};
use crate::openhuman::agent::harness::subagent_runner::{self, SubagentRunOptions};
use crate::openhuman::agent::Agent;
use crate::openhuman::config::Config;
use crate::openhuman::memory::tree::canonicalize::document::DocumentInput;
use crate::openhuman::memory::tree::ingest::{self as tree_ingest, IngestResult};

use super::decision::TriageAction;
use super::envelope::TriggerEnvelope;
use super::evaluator::TriageRun;
use super::events;

/// Cap on the JSON payload preview embedded in the ACK memo. Long
/// payloads (a 200-message Slack thread, a 50 KB Gmail body) would
/// otherwise dominate the canonicaliser's chunker output.
const ACK_PAYLOAD_PREVIEW_MAX_CHARS: usize = 1024;

/// Tag attached to every triage ACK memo so retrieval can scope a
/// query (`tags:["triage:acknowledge"]`) to just these rows.
const ACK_TRIAGE_TAG: &str = "triage:acknowledge";

/// Owner field on the memory row. The closedhuman fork is single-user
/// local; `"self"` matches the convention used by vault sync.
const ACK_MEMORY_OWNER: &str = "self";

/// Executes the side effects of a triage decision.
///
/// This function is responsible for:
/// 1. Publishing the `TriggerEvaluated` telemetry event.
/// 2. Logging the classification outcome.
/// 3. If the action is `React` or `Escalate`, dispatching the appropriate
///    sub-agent (`trigger_reactor` or `orchestrator`).
/// 4. Publishing `TriggerEscalated` or `TriggerEscalationFailed` events.
pub async fn apply_decision(run: TriageRun, envelope: &TriggerEnvelope) -> anyhow::Result<()> {
    // Always publish `TriggerEvaluated` — it's the single source of
    // truth for dashboards, counts every trigger regardless of action.
    events::publish_evaluated(
        envelope,
        run.decision.action.as_str(),
        run.used_local,
        run.latency_ms,
    );

    match run.decision.action {
        TriageAction::Drop => {
            tracing::debug!(
                label = %envelope.display_label,
                external_id = %envelope.external_id,
                reason = %run.decision.reason,
                "[triage::escalation] DROP — no downstream work"
            );
        }
        TriageAction::Acknowledge => {
            tracing::info!(
                label = %envelope.display_label,
                external_id = %envelope.external_id,
                reason = %run.decision.reason,
                "[triage::escalation] ACKNOWLEDGE — logged"
            );
            persist_acknowledge_memory(envelope, &run.decision.reason).await;
        }
        TriageAction::React | TriageAction::Escalate => {
            let target = run
                .decision
                .target_agent
                .as_deref()
                .unwrap_or("trigger_reactor");
            let prompt = run.decision.prompt.as_deref().unwrap_or("");
            let action_str = run.decision.action.as_str().to_uppercase();

            tracing::info!(
                action = %action_str,
                target_agent = %target,
                label = %envelope.display_label,
                external_id = %envelope.external_id,
                prompt_chars = prompt.chars().count(),
                reason = %run.decision.reason,
                "[triage::escalation] dispatching sub-agent"
            );

            match dispatch_target_agent(target, prompt).await {
                Ok(output) => {
                    tracing::info!(
                        target_agent = %target,
                        output_chars = output.chars().count(),
                        "[triage::escalation] sub-agent completed"
                    );
                    events::publish_escalated(envelope, target);
                }
                Err(err) => {
                    tracing::error!(
                        target_agent = %target,
                        error = %err,
                        "[triage::escalation] sub-agent dispatch failed"
                    );
                    events::publish_failed(
                        envelope,
                        &format!("sub-agent `{target}` failed: {err}"),
                    );
                    return Err(err);
                }
            }
        }
    }
    Ok(())
}

/// Build a full [`Agent`] from config, install a [`ParentExecutionContext`]
/// on the task-local, and call [`run_subagent`] with the named definition
/// and prompt.
///
/// This is heavier than a simple `agent.run_turn` bus call — it creates a
/// provider, memory store, tool registry, and all the machinery `Agent`
/// normally needs. The cost is acceptable because `react`/`escalate`
/// triggers are relatively rare (most triggers are `drop`/`acknowledge`)
/// and the construction is the same O(1) code path `agent_chat` uses.
async fn dispatch_target_agent(agent_id: &str, prompt: &str) -> anyhow::Result<String> {
    // Validate the target agent id BEFORE loading the workspace config
    // or building the `Agent` — both are expensive (config reads
    // ~/.openhuman, Agent::from_config touches the provider factory,
    // composio cache, etc.) and a bad agent id can be rejected for
    // free. Fail-fast also makes the error message mention the
    // offending id, which is what callers + tests assert on.
    let registry = AgentDefinitionRegistry::global()
        .ok_or_else(|| anyhow!("AgentDefinitionRegistry not initialised"))?;
    let definition = registry
        .get(agent_id)
        .ok_or_else(|| anyhow!("agent definition `{agent_id}` not found in registry"))?;

    let config = Config::load_or_init()
        .await
        .context("loading config for sub-agent dispatch")?;

    let mut agent =
        Agent::from_config(&config).context("building Agent from config for sub-agent dispatch")?;

    // Populate connected integrations from the process-wide cache (or a
    // fresh fetch if cold) so triage-triggered sub-agents see the real
    // integrations in their system prompts.
    let integrations = crate::openhuman::composio::fetch_connected_integrations(&config).await;
    agent.set_connected_integrations(integrations);

    // Build the ParentExecutionContext from the Agent's public accessors
    // so `run_subagent` can inherit the provider, tools, memory, etc.
    let parent_ctx = ParentExecutionContext {
        provider: agent.provider_arc(),
        all_tools: agent.tools_arc(),
        all_tool_specs: agent.tool_specs_arc(),
        model_name: agent.model_name().to_string(),
        temperature: agent.temperature(),
        workspace_dir: agent.workspace_dir().to_path_buf(),
        memory: agent.memory_arc(),
        agent_config: agent.agent_config().clone(),
        skills: Arc::new(agent.skills().to_vec()),
        memory_context: Arc::new(None), // Sub-agent queries memory via tools if needed
        session_id: format!("triage-{}", uuid::Uuid::new_v4()),
        channel: "triage".to_string(),
        connected_integrations: agent.connected_integrations().to_vec(),
        // Triage runs sub-agents with the parent's existing dispatcher
        // — fall back to PFormat if no accessor is available. Triage
        // doesn't currently spawn anything that depends on the new
        // dispatcher-aware sub-agent renderer.
        tool_call_format: crate::openhuman::context::prompt::ToolCallFormat::PFormat,
        // Triage inherits the parent's session-key chain so escalated
        // sub-agents write their transcripts alongside the parent's,
        // preserving the `{parent}__{child}.jsonl` hierarchy.
        session_key: agent.session_key().to_string(),
        session_parent_prefix: agent.session_parent_prefix().map(str::to_string),
        // Triage runs sub-agents synchronously without streaming progress
        // back to a UI; the runner skips child-progress emission when this
        // is `None`.
        on_progress: None,
    };

    tracing::debug!(
        agent_id = %agent_id,
        model = %parent_ctx.model_name,
        tool_count = parent_ctx.all_tools.len(),
        "[triage::escalation] dispatching run_subagent with parent context"
    );

    let outcome = with_parent_context(parent_ctx, async {
        subagent_runner::run_subagent(definition, prompt, SubagentRunOptions::default()).await
    })
    .await
    .map_err(|e| anyhow!("run_subagent(`{agent_id}`) failed: {e}"))?;

    tracing::debug!(
        agent_id = %agent_id,
        elapsed_ms = outcome.elapsed.as_millis() as u64,
        iterations = outcome.iterations,
        output_chars = outcome.output.chars().count(),
        "[triage::escalation] run_subagent completed"
    );

    Ok(outcome.output)
}

/// Load config and write an ACK memo to the memory tree.
///
/// Best-effort: a failure to load config or ingest must not fail the
/// triage pipeline. Both branches log a warning and return.
async fn persist_acknowledge_memory(envelope: &TriggerEnvelope, reason: &str) {
    let config = match Config::load_or_init().await {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::warn!(
                error = %err,
                label = %envelope.display_label,
                external_id = %envelope.external_id,
                "[triage::escalation] ACK memory-write skipped — config load failed"
            );
            return;
        }
    };

    match write_acknowledge_memory(&config, envelope, reason).await {
        Ok(result) => {
            tracing::debug!(
                label = %envelope.display_label,
                external_id = %envelope.external_id,
                source_id = %result.source_id,
                chunks_written = result.chunks_written,
                already_ingested = result.already_ingested,
                "[triage::escalation] ACK memory-write committed"
            );
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                label = %envelope.display_label,
                external_id = %envelope.external_id,
                "[triage::escalation] ACK memory-write failed (non-fatal)"
            );
        }
    }
}

/// Build the `DocumentInput` for an ACK memo and hand it to
/// [`tree_ingest::ingest_document`].
///
/// `source_id` is deterministic on `triage-ack:<source-slug>:<external_id>`
/// so Composio retries (and other re-deliveries that share `external_id`)
/// short-circuit via `tree::ingest::already_ingested` instead of duplicating
/// the memo into the tree.
async fn write_acknowledge_memory(
    config: &Config,
    envelope: &TriggerEnvelope,
    reason: &str,
) -> anyhow::Result<IngestResult> {
    let source_slug = envelope.source.slug();
    let source_id = format!("triage-ack:{source_slug}:{}", envelope.external_id);

    let body = build_acknowledge_body(envelope, reason);

    let doc = DocumentInput {
        provider: format!("triage-ack/{source_slug}"),
        title: envelope.display_label.clone(),
        body,
        modified_at: envelope.received_at,
        source_ref: Some(format!(
            "triage-ack://{source_slug}/{}",
            envelope.external_id
        )),
    };

    let tags = vec![ACK_TRIAGE_TAG.to_string(), format!("source:{source_slug}")];

    tree_ingest::ingest_document(config, &source_id, ACK_MEMORY_OWNER, tags, doc)
        .await
        .context("triage::escalation: writing ACK memo to memory tree")
}

/// Compose the markdown body for an ACK memo. Always emits the label
/// and received-at timestamp so the canonicaliser never sees an empty
/// body; the LLM reason and a payload preview are appended when
/// non-trivial.
fn build_acknowledge_body(envelope: &TriggerEnvelope, reason: &str) -> String {
    let mut body = String::new();
    let _ = writeln!(&mut body, "Triggered event: {}", envelope.display_label);
    let _ = writeln!(
        &mut body,
        "Received at: {}",
        envelope.received_at.to_rfc3339()
    );

    let trimmed_reason = reason.trim();
    if !trimmed_reason.is_empty() {
        let _ = writeln!(&mut body);
        let _ = writeln!(&mut body, "Triage reasoning:");
        let _ = writeln!(&mut body, "{trimmed_reason}");
    }

    if let Ok(payload_str) = serde_json::to_string(&envelope.payload) {
        if !payload_str.is_empty() && payload_str != "null" && payload_str != "{}" {
            let preview = truncate_chars(&payload_str, ACK_PAYLOAD_PREVIEW_MAX_CHARS);
            let _ = writeln!(&mut body);
            let _ = writeln!(&mut body, "Payload preview:");
            let _ = writeln!(&mut body, "{preview}");
        }
    }

    body
}

/// Truncate by char count (not byte count) so multi-byte codepoints
/// can't be split. Appends a single horizontal-ellipsis when truncation
/// occurred so it's visible in the memo body.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let total: usize = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event_bus::{init_global, DomainEvent};
    use crate::openhuman::agent::harness::definition::AgentDefinitionRegistry;
    use crate::openhuman::memory::tree::store::{count_chunks, list_chunks, ListChunksQuery};
    use crate::openhuman::memory::tree::types::SourceKind;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::time::{sleep, timeout, Duration};

    static TEST_EVENTS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct TestEventsGuard(tokio::sync::MutexGuard<'static, ()>);

    impl Drop for TestEventsGuard {
        fn drop(&mut self) {
            events::clear_test_events();
        }
    }

    async fn test_events_guard() -> TestEventsGuard {
        let guard = TEST_EVENTS_LOCK.lock().await;
        events::clear_test_events();
        TestEventsGuard(guard)
    }

    fn envelope(external_id: &str) -> TriggerEnvelope {
        TriggerEnvelope::from_composio(
            "gmail",
            "GMAIL_NEW_GMAIL_MESSAGE",
            "triage-escalation",
            external_id,
            json!({ "subject": "hello" }),
        )
    }

    fn run(action: TriageAction) -> TriageRun {
        TriageRun {
            decision: super::super::decision::TriageDecision {
                action,
                target_agent: None,
                prompt: None,
                reason: "because".into(),
            },
            used_local: false,
            latency_ms: 9,
            resolution_path: super::super::evaluator::TriageResolutionPath::Cloud,
        }
    }

    fn run_with_target(action: TriageAction, target_agent: &str, prompt: &str) -> TriageRun {
        TriageRun {
            decision: super::super::decision::TriageDecision {
                action,
                target_agent: Some(target_agent.into()),
                prompt: Some(prompt.into()),
                reason: "because".into(),
            },
            used_local: false,
            latency_ms: 9,
            resolution_path: super::super::evaluator::TriageResolutionPath::Cloud,
        }
    }

    async fn collect_trigger_events_until(
        external_id: &str,
        expected: impl Fn(&[DomainEvent]) -> bool,
    ) -> Vec<DomainEvent> {
        let external_id = external_id.to_string();
        timeout(Duration::from_secs(5), async {
            loop {
                let captured = events::test_events_for_external_id(&external_id);
                if expected(&captured) {
                    return captured;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("expected triage event should arrive")
    }

    #[tokio::test]
    async fn apply_decision_drop_only_publishes_evaluated() {
        let _events_guard = test_events_guard().await;
        let envelope = envelope("esc-drop");
        let _ = init_global(32);
        let collect = tokio::spawn(collect_trigger_events_until("esc-drop", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    DomainEvent::TriggerEvaluated {
                        decision,
                        external_id,
                        ..
                    } if decision == "drop" && external_id == "esc-drop"
                )
            })
        }));

        apply_decision(run(TriageAction::Drop), &envelope)
            .await
            .expect("drop should not fail");

        let captured = collect.await.expect("event collector should not panic");
        assert!(captured.iter().any(|event| matches!(
            event,
            DomainEvent::TriggerEvaluated {
                decision,
                external_id,
                ..
            } if decision == "drop" && external_id == "esc-drop"
        )));
        assert!(!captured.iter().any(|event| matches!(
            event,
            DomainEvent::TriggerEscalated { external_id, .. }
                | DomainEvent::TriggerEscalationFailed { external_id, .. }
                if external_id == "esc-drop"
        )));
    }

    #[tokio::test]
    async fn apply_decision_acknowledge_only_publishes_evaluated() {
        let _events_guard = test_events_guard().await;
        let envelope = envelope("esc-ack");
        let _ = init_global(32);
        let collect = tokio::spawn(collect_trigger_events_until("esc-ack", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    DomainEvent::TriggerEvaluated {
                        decision,
                        external_id,
                        ..
                    } if decision == "acknowledge" && external_id == "esc-ack"
                )
            })
        }));

        apply_decision(run(TriageAction::Acknowledge), &envelope)
            .await
            .expect("acknowledge should not fail");

        let captured = collect.await.expect("event collector should not panic");
        assert!(captured.iter().any(|event| matches!(
            event,
            DomainEvent::TriggerEvaluated {
                decision,
                external_id,
                ..
            } if decision == "acknowledge" && external_id == "esc-ack"
        )));
        assert!(!captured.iter().any(|event| matches!(
            event,
            DomainEvent::TriggerEscalated { external_id, .. }
                | DomainEvent::TriggerEscalationFailed { external_id, .. }
                if external_id == "esc-ack"
        )));
    }

    #[tokio::test]
    async fn apply_decision_react_failure_publishes_failed_event() {
        let _events_guard = test_events_guard().await;
        let envelope = envelope("esc-react-fail");
        let _ = init_global(32);
        let _ = AgentDefinitionRegistry::init_global_builtins();
        let collect = tokio::spawn(collect_trigger_events_until("esc-react-fail", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    DomainEvent::TriggerEvaluated {
                        decision,
                        external_id,
                        ..
                    } if decision == "react" && external_id == "esc-react-fail"
                )
            }) && events.iter().any(|event| {
                matches!(
                    event,
                    DomainEvent::TriggerEscalationFailed { external_id, reason, .. }
                        if external_id == "esc-react-fail" && reason.contains("missing-agent")
                )
            })
        }));

        let err = apply_decision(
            run_with_target(TriageAction::React, "missing-agent", "handle this"),
            &envelope,
        )
        .await
        .expect_err("missing target agent should fail");
        assert!(err.to_string().contains("missing-agent"));

        let captured = collect.await.expect("event collector should not panic");
        assert!(captured.iter().any(|event| matches!(
            event,
            DomainEvent::TriggerEvaluated {
                decision,
                external_id,
                ..
            } if decision == "react" && external_id == "esc-react-fail"
        )));
        assert!(captured.iter().any(|event| matches!(
            event,
            DomainEvent::TriggerEscalationFailed { external_id, reason, .. }
                if external_id == "esc-react-fail" && reason.contains("missing-agent")
        )));
    }

    #[tokio::test]
    async fn apply_decision_escalate_failure_publishes_failed_event() {
        let _events_guard = test_events_guard().await;
        let envelope = envelope("esc-escalate-fail");
        let _ = init_global(32);
        let _ = AgentDefinitionRegistry::init_global_builtins();
        let collect = tokio::spawn(collect_trigger_events_until(
            "esc-escalate-fail",
            |events| {
                events.iter().any(|event| {
                    matches!(
                event,
                DomainEvent::TriggerEvaluated {
                    decision,
                    external_id,
                    ..
                } if decision == "escalate" && external_id == "esc-escalate-fail"
            )
                }) && events.iter().any(|event| matches!(
                event,
                DomainEvent::TriggerEscalationFailed { external_id, reason, .. }
                    if external_id == "esc-escalate-fail" && reason.contains("missing-agent")
            ))
            },
        ));

        let err = apply_decision(
            run_with_target(TriageAction::Escalate, "missing-agent", "escalate this"),
            &envelope,
        )
        .await
        .expect_err("missing orchestrator target should fail");
        assert!(err.to_string().contains("missing-agent"));

        let captured = collect.await.expect("event collector should not panic");
        assert!(captured.iter().any(|event| matches!(
            event,
            DomainEvent::TriggerEvaluated {
                decision,
                external_id,
                ..
            } if decision == "escalate" && external_id == "esc-escalate-fail"
        )));
        assert!(captured.iter().any(|event| matches!(
            event,
            DomainEvent::TriggerEscalationFailed { external_id, reason, .. }
                if external_id == "esc-escalate-fail" && reason.contains("missing-agent")
        )));
    }

    // ---------- ACK memory-write ----------

    fn ack_test_config() -> (TempDir, Config) {
        let tmp = TempDir::new().unwrap();
        let mut cfg = Config::default();
        cfg.workspace_dir = tmp.path().to_path_buf();
        cfg.memory_tree.embedding_endpoint = None;
        cfg.memory_tree.embedding_model = None;
        cfg.memory_tree.embedding_strict = false;
        (tmp, cfg)
    }

    #[tokio::test]
    async fn write_acknowledge_memory_persists_a_chunk_under_triage_ack_source_id() {
        let (_tmp, cfg) = ack_test_config();
        let envelope = TriggerEnvelope::from_composio(
            "github",
            "GITHUB_COMMIT_EVENT",
            "ack-write-meta-id",
            "ack-write-uuid",
            json!({
                "repository": "Jakedismo/openhuman-in-my-taste",
                "commit": "abcdef1",
                "message": "fix(triage): write ACK memo to memory tree",
            }),
        );

        let result = write_acknowledge_memory(
            &cfg,
            &envelope,
            "Technical commit; acknowledge for later recall.",
        )
        .await
        .expect("ACK ingest should succeed");

        assert_eq!(result.source_id, "triage-ack:composio:ack-write-uuid");
        assert!(
            !result.already_ingested,
            "first ACK write should not short-circuit"
        );
        assert!(
            result.chunks_written >= 1,
            "ACK memo body must produce at least one chunk"
        );
        assert_eq!(count_chunks(&cfg).unwrap(), result.chunks_written as u64);

        let rows = list_chunks(&cfg, &ListChunksQuery::default()).unwrap();
        let row = rows
            .iter()
            .find(|r| r.metadata.source_id == "triage-ack:composio:ack-write-uuid")
            .expect("ACK row should be present");
        assert_eq!(row.metadata.source_kind, SourceKind::Document);
        assert_eq!(row.metadata.owner, ACK_MEMORY_OWNER);
        assert!(
            row.metadata.tags.iter().any(|t| t == ACK_TRIAGE_TAG),
            "ACK row must carry triage:acknowledge tag (got: {:?})",
            row.metadata.tags
        );
        assert!(
            row.metadata.tags.iter().any(|t| t == "source:composio"),
            "ACK row must carry source slug tag (got: {:?})",
            row.metadata.tags
        );
    }

    #[tokio::test]
    async fn write_acknowledge_memory_is_idempotent_on_repeated_external_id() {
        let (_tmp, cfg) = ack_test_config();
        let envelope = TriggerEnvelope::from_composio(
            "gmail",
            "GMAIL_NEW_GMAIL_MESSAGE",
            "ack-idem-meta",
            "ack-idem-uuid",
            json!({ "subject": "Order confirmation", "from": "shop@example.com" }),
        );

        let first = write_acknowledge_memory(&cfg, &envelope, "FYI, nothing to act on.")
            .await
            .expect("first ACK ingest should succeed");
        assert!(!first.already_ingested);
        assert!(first.chunks_written >= 1);
        let initial_count = count_chunks(&cfg).unwrap();

        // Composio retry: same metadata.uuid, possibly different payload
        // detail. Must not duplicate the memo into the tree.
        let mut retry = envelope.clone();
        retry.payload = json!({
            "subject": "Order confirmation (retry)",
            "from": "shop@example.com",
        });
        let second = write_acknowledge_memory(&cfg, &retry, "FYI, nothing to act on.")
            .await
            .expect("retry ACK ingest should succeed");
        assert!(second.already_ingested);
        assert_eq!(second.chunks_written, 0);
        assert_eq!(count_chunks(&cfg).unwrap(), initial_count);
    }

    #[test]
    fn build_acknowledge_body_includes_label_and_received_at_even_when_payload_empty() {
        let envelope = TriggerEnvelope::from_external("caller-x", "manual", json!({}));
        let body = build_acknowledge_body(&envelope, "");

        assert!(body.contains("Triggered event: external/caller-x"));
        assert!(body.contains("Received at: "));
        assert!(
            !body.contains("Triage reasoning:"),
            "empty reason must not emit the heading"
        );
        assert!(
            !body.contains("Payload preview:"),
            "empty payload object must not emit the heading"
        );
    }

    #[test]
    fn build_acknowledge_body_includes_reason_and_payload_preview() {
        let envelope = TriggerEnvelope::from_composio(
            "github",
            "GITHUB_COMMIT_EVENT",
            "m",
            "u",
            json!({ "message": "ship it" }),
        );
        let body = build_acknowledge_body(&envelope, "Test commit landed.");
        assert!(body.contains("Triage reasoning:"));
        assert!(body.contains("Test commit landed."));
        assert!(body.contains("Payload preview:"));
        assert!(body.contains("ship it"));
    }

    #[test]
    fn truncate_chars_respects_multibyte_codepoint_boundaries() {
        let s: String = std::iter::repeat('猫').take(200).collect();
        let truncated = truncate_chars(&s, 10);
        // 10 cats + ellipsis (each char is multi-byte UTF-8)
        assert_eq!(truncated.chars().count(), 11);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_chars_passes_through_short_strings() {
        assert_eq!(truncate_chars("short", 16), "short");
    }
}
