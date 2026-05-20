// ABOUTME: Composio webhook subscription lifecycle — create / reuse / patch.
// ABOUTME: Single global subscription per user; persists across restarts.

//! # Webhook subscription helper
//!
//! When direct-mode triggers come online we need exactly ONE
//! webhook subscription at app.composio.dev pointing at our ngrok
//! tunnel. This module owns that lifecycle:
//!
//! - First call to [`ensure_subscription`] with no remembered ID →
//!   POSTs to `/api/v3/webhook_subscriptions`, persists the returned
//!   `{id, secret}` pair into config + AuthService.
//! - Subsequent calls with a remembered ID → GET to confirm the
//!   subscription still exists, PATCH to add any missing event types
//!   to `enabled_events`.
//! - Caller drove tunnel URL changed (user rotated ngrok domain) →
//!   PATCH to update `webhook_url`.
//!
//! See the architecture decisions in
//! `~/.claude/plans/fancy-meandering-cake.md` ("Decisions locked" /
//! item 2 — single global subscription covering all event types).

use std::sync::Arc;

use crate::openhuman::config::Config;
use crate::openhuman::credentials::ops::{
    get_composio_webhook_secret, store_composio_webhook_secret,
};
use crate::openhuman::tools::ComposioTool;

/// Resolved subscription state — what the receiver needs to keep
/// running. Both fields are required for an active receiver: `id` so
/// we can PATCH on future enable_trigger calls, `secret` so the
/// receiver can HMAC-verify inbound deliveries.
#[derive(Debug, Clone)]
pub struct ResolvedSubscription {
    pub id: String,
    pub secret: String,
}

/// Outcome of [`ensure_subscription`]. Distinguishes the three paths
/// so the caller can emit accurate log lines and so tests can assert
/// the right branch ran.
#[derive(Debug, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// No subscription existed; we created a fresh one.
    Created,
    /// Existing subscription is fine as-is; we reused it without any
    /// upstream call.
    ReusedExisting,
    /// Existing subscription needed updating (new event types added,
    /// or webhook_url rotated) — we PATCHed it.
    Patched,
}

/// Bring the Composio webhook subscription into the desired state.
///
/// `webhook_url`: the public HTTPS URL the receiver listens on
/// (e.g. `https://<id>.ngrok-free.dev/webhook`). MUST be HTTPS —
/// Composio rejects non-HTTPS subscriptions at create time.
///
/// `desired_event_types`: the set of event types this subscription
/// must cover. On reuse, missing types trigger a PATCH; extras are
/// left in place (we don't garbage-collect since other triggers in
/// the same OpenHuman install may have added them).
///
/// `remembered_subscription_id`: the persisted ID from
/// `config.composio.webhook.composio_webhook_subscription_id`. Empty
/// string means "no remembered subscription, create one".
///
/// Returns the resolved subscription (`id`, `secret`) plus the
/// outcome enum so the caller can persist the ID back into config
/// when it changed, and emit the right log line.
pub async fn ensure_subscription(
    config: &Config,
    direct: &Arc<ComposioTool>,
    webhook_url: &str,
    desired_event_types: &[String],
    remembered_subscription_id: &str,
) -> anyhow::Result<(ResolvedSubscription, EnsureOutcome)> {
    let webhook_url = webhook_url.trim();
    if webhook_url.is_empty() {
        anyhow::bail!("composio ensure_subscription: webhook_url must not be empty");
    }
    if !webhook_url.starts_with("https://") {
        anyhow::bail!(
            "composio ensure_subscription: webhook_url must be HTTPS (got {webhook_url})"
        );
    }
    if desired_event_types.is_empty() {
        anyhow::bail!("composio ensure_subscription: at least one event type must be requested");
    }

    let remembered_id = remembered_subscription_id.trim();
    let remembered_secret = get_composio_webhook_secret(config).map_err(|e| {
        anyhow::anyhow!("composio ensure_subscription: failed to read webhook secret: {e}")
    })?;

    if !remembered_id.is_empty() {
        if let Some(secret) = remembered_secret.as_deref() {
            match direct.get_webhook_subscription_v3(remembered_id).await {
                Ok(existing) => {
                    let existing_events = extract_event_set(&existing);
                    let existing_url = existing
                        .get("webhook_url")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .trim();
                    let missing_events: Vec<String> = desired_event_types
                        .iter()
                        .filter(|e| !existing_events.iter().any(|x| x == *e))
                        .cloned()
                        .collect();
                    let url_changed = existing_url != webhook_url;

                    if missing_events.is_empty() && !url_changed {
                        tracing::debug!(
                            subscription_id = remembered_id,
                            "[composio-webhook] ensure_subscription: reusing existing subscription"
                        );
                        return Ok((
                            ResolvedSubscription {
                                id: remembered_id.to_string(),
                                secret: secret.to_string(),
                            },
                            EnsureOutcome::ReusedExisting,
                        ));
                    }

                    let merged_events: Vec<String> = existing_events
                        .into_iter()
                        .chain(missing_events.into_iter())
                        .collect();
                    let merged_events: Vec<String> = dedupe_preserving_order(merged_events);

                    let new_url = if url_changed { Some(webhook_url) } else { None };
                    direct
                        .update_webhook_subscription_v3(
                            remembered_id,
                            new_url,
                            Some(&merged_events),
                        )
                        .await?;
                    tracing::debug!(
                        subscription_id = remembered_id,
                        url_changed,
                        events_after = merged_events.len(),
                        "[composio-webhook] ensure_subscription: patched existing subscription"
                    );
                    return Ok((
                        ResolvedSubscription {
                            id: remembered_id.to_string(),
                            secret: secret.to_string(),
                        },
                        EnsureOutcome::Patched,
                    ));
                }
                Err(err) => {
                    // 404 (or any get failure) → fall through to
                    // create. We do NOT swallow non-404 errors
                    // silently because that would mask transient
                    // outages. The create attempt below will surface
                    // any persistent failure to the caller.
                    tracing::warn!(
                        subscription_id = remembered_id,
                        error = %err,
                        "[composio-webhook] ensure_subscription: remembered subscription not retrievable, will create a new one"
                    );
                }
            }
        } else {
            tracing::warn!(
                subscription_id = remembered_id,
                "[composio-webhook] ensure_subscription: remembered subscription id present but no stored secret, recreating"
            );
        }
    }

    let resp = direct
        .create_webhook_subscription_v3(webhook_url, desired_event_types)
        .await?;
    let id = resp
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let secret = resp
        .get("secret")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if id.is_empty() || secret.is_empty() {
        anyhow::bail!(
            "composio ensure_subscription: create response missing id or secret (got {resp})"
        );
    }
    store_composio_webhook_secret(config, &secret).map_err(|e| {
        anyhow::anyhow!("composio ensure_subscription: failed to persist webhook secret: {e}")
    })?;
    tracing::debug!(
        subscription_id = id,
        events = desired_event_types.len(),
        "[composio-webhook] ensure_subscription: created new subscription"
    );
    Ok((ResolvedSubscription { id, secret }, EnsureOutcome::Created))
}

/// Extract `enabled_events` from a v3 subscription envelope as a set
/// of canonical event type strings. Handles both the camelCase and
/// snake_case payloads (Composio's response shape is consistent but
/// the spec allows either historically — be defensive).
fn extract_event_set(envelope: &serde_json::Value) -> Vec<String> {
    let arr = envelope
        .get("enabled_events")
        .or_else(|| envelope.get("enabledEvents"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    arr.into_iter()
        .filter_map(|v| {
            v.as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// Preserve insertion order while removing duplicates. We avoid
/// `HashSet` so the resulting `enabled_events` list is deterministic
/// across runs — matters for test fixtures and for not churning
/// Composio's record needlessly.
fn dedupe_preserving_order(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

#[cfg(test)]
#[path = "subscription_test.rs"]
mod tests;
