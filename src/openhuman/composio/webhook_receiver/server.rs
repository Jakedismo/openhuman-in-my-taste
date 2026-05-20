// ABOUTME: Axum receiver for Composio webhook deliveries — HMAC verify + bus dispatch.
// ABOUTME: Binds to 127.0.0.1; ngrok provides the public-facing HTTPS terminus.

//! # Local webhook server
//!
//! Single POST `/webhook` endpoint receiving Composio trigger
//! deliveries forwarded from the ngrok tunnel. The handler:
//!
//! 1. Extracts the Svix-style signing headers (`webhook-id`,
//!    `webhook-timestamp`, `webhook-signature`).
//! 2. Resolves the per-subscription secret from `AuthService` (one
//!    call per request — keeps the secret out of the binding context
//!    so rotations propagate without a server restart).
//! 3. HMAC-verifies the body via [`super::hmac::verify`].
//! 4. Parses the body into
//!    [`crate::openhuman::composio::types::ComposioTriggerEvent`].
//! 5. Publishes
//!    [`crate::core::event_bus::DomainEvent::ComposioTriggerReceived`]
//!    so the existing `trigger_triage` / `trigger_reactor` pipeline
//!    picks up the event unchanged.
//!
//! Health probe: GET `/healthz` returns `200 OK` with body
//! `composio-webhook-receiver: ok`. Used by the "Test tunnel" button
//! in Settings → Triggers (no auth, no HMAC — just confirms the
//! tunnel is wired correctly).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;

use crate::core::event_bus::{publish_global, DomainEvent};
use crate::openhuman::config::Config;
use crate::openhuman::credentials::ops::get_composio_webhook_secret;

use serde::Deserialize;

/// Composio v3 webhook envelope. ALL trigger deliveries arrive as
/// `type = "composio.trigger.message"` with the actual trigger slug
/// in `metadata.trigger_slug` — this differs from the legacy backend-
/// relayed `ComposioTriggerEvent` DTO that carried `toolkit` and
/// `trigger` at the top level.
///
/// Reference: `WebhookTriggerPayloadV3` in
/// [`python/composio/core/models/triggers.py`](https://github.com/ComposioHQ/composio/blob/next/python/composio/core/models/triggers.py)
/// and the matching Zod schema at
/// [`ts/packages/core/src/types/triggers.types.ts`](https://github.com/ComposioHQ/composio/blob/next/ts/packages/core/src/types/triggers.types.ts).
#[derive(Debug, Deserialize)]
struct WebhookEnvelopeV3 {
    /// Event id — same value Composio puts in the `webhook-id` header.
    #[serde(default)]
    id: String,
    /// `composio.trigger.message`, `composio.connected_account.expired`,
    /// or any future `composio.*` type.
    #[serde(default, rename = "type")]
    event_type: String,
    /// Trigger-event-specific fields. Connection events use a different
    /// metadata shape, but we don't need their fields today — leaving
    /// `WebhookConnectionMetadata` unmodeled until we wire a domain
    /// event for it.
    #[serde(default)]
    metadata: WebhookTriggerMetadataV3,
    /// The actual trigger payload that the toolkit emits (e.g. for
    /// `GMAIL_NEW_GMAIL_MESSAGE` the parsed message envelope). Opaque
    /// to the receiver — handed through to subscribers verbatim.
    #[serde(default)]
    data: serde_json::Value,
}

#[derive(Debug, Default, Deserialize)]
struct WebhookTriggerMetadataV3 {
    /// Per-delivery unique id — useful for trigger-history dedup.
    #[serde(default)]
    log_id: String,
    /// Canonical trigger slug, e.g. `GMAIL_NEW_GMAIL_MESSAGE`.
    #[serde(default)]
    trigger_slug: String,
    /// Trigger instance id (the nano id returned by `upsert`).
    #[serde(default)]
    trigger_id: String,
    /// Toolkit-derivation source per the upstream TS reference impl:
    /// `triggerSlug.split('_')[0].toUpperCase()`. Composio also
    /// frequently sends an explicit `toolkit` field in metadata, so
    /// accept that when present and only fall back to the derivation.
    #[serde(default)]
    toolkit: String,
}

use super::hmac::{verify, VerifyError, DEFAULT_TIMESTAMP_TOLERANCE_SECS};

/// Shared state carried into the `/webhook` handler. `config` is
/// shared via Arc so the AuthService lookup for the webhook secret
/// can happen per request (necessary for rotation to take effect
/// without restart).
#[derive(Clone)]
pub struct ReceiverState {
    pub config: Arc<Config>,
}

/// Build the Axum router. Pure constructor — does NOT bind a socket;
/// the caller (`super::tunnel` lifecycle code) is responsible for
/// `axum::serve` once it has chosen a port.
pub fn build_router(state: ReceiverState) -> Router {
    Router::new()
        .route("/webhook", post(handle_webhook))
        .route("/healthz", get(handle_healthz))
        .with_state(state)
}

async fn handle_healthz() -> impl IntoResponse {
    (StatusCode::OK, "composio-webhook-receiver: ok")
}

/// Inbound webhook handler. Returns:
///
/// - `200 OK` on a verified delivery dispatched to the bus.
/// - `400 BAD REQUEST` on malformed headers / body shape — caller
///   should not retry.
/// - `401 UNAUTHORIZED` on HMAC mismatch or out-of-window timestamp.
///   Composio retries 401s on a backoff, which is acceptable for
///   the transient "secret being rotated" case; in steady state a
///   401 indicates an attack attempt and the client will eventually
///   give up.
/// - `503 SERVICE UNAVAILABLE` when no webhook secret is stored —
///   indicates the subscription hasn't been set up yet. Composio
///   should be holding its retries during this window.
async fn handle_webhook(
    State(state): State<ReceiverState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let secret = match get_composio_webhook_secret(&state.config) {
        Ok(Some(s)) => s,
        Ok(None) => {
            tracing::warn!(
                "[composio-webhook] received delivery but no subscription secret is stored; \
                 returning 503 — Composio will retry"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "composio webhook receiver: no subscription secret",
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "[composio-webhook] failed to read subscription secret");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "composio webhook receiver: secret store unavailable",
            )
                .into_response();
        }
    };

    let webhook_id = headers.get("webhook-id").and_then(|v| v.to_str().ok());
    let webhook_timestamp = headers
        .get("webhook-timestamp")
        .and_then(|v| v.to_str().ok());
    let webhook_signature = headers
        .get("webhook-signature")
        .and_then(|v| v.to_str().ok());

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    match verify(
        webhook_id,
        webhook_timestamp,
        webhook_signature,
        body.as_ref(),
        secret.as_bytes(),
        now,
        DEFAULT_TIMESTAMP_TOLERANCE_SECS,
    ) {
        Ok(()) => {}
        Err(VerifyError::MissingHeader(_)) | Err(VerifyError::InvalidTimestamp) => {
            tracing::warn!("[composio-webhook] rejecting delivery: malformed headers");
            return (
                StatusCode::BAD_REQUEST,
                "missing or malformed signing headers",
            )
                .into_response();
        }
        Err(VerifyError::NoV1Signatures) => {
            tracing::warn!("[composio-webhook] rejecting delivery: no v1 signature tokens");
            return (StatusCode::BAD_REQUEST, "no usable v1 signature").into_response();
        }
        Err(VerifyError::TimestampOutOfWindow { delta_secs, .. }) => {
            tracing::warn!(
                delta_secs,
                "[composio-webhook] rejecting delivery: timestamp outside tolerance window"
            );
            return (
                StatusCode::UNAUTHORIZED,
                "webhook timestamp outside tolerance window",
            )
                .into_response();
        }
        Err(VerifyError::SignatureMismatch) => {
            tracing::warn!(
                "[composio-webhook] rejecting delivery: HMAC signature mismatch \
                 (likely tampered or wrong secret)"
            );
            return (StatusCode::UNAUTHORIZED, "signature mismatch").into_response();
        }
        Err(VerifyError::InvalidSecret) => {
            tracing::error!(
                "[composio-webhook] HMAC primitive rejected the stored secret — \
                 secret is empty or otherwise unusable"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "composio webhook receiver: stored secret is unusable",
            )
                .into_response();
        }
    }

    // HMAC verified — parse the v3 envelope. Composio sends
    // `{id, timestamp, type, metadata, data}` where `type` is one of
    // a fixed set of `composio.*` event types. We branch on `type`:
    // - `composio.trigger.message` → publish ComposioTriggerReceived
    //   so the existing trigger_triage pipeline picks it up unchanged.
    // - `composio.connected_account.expired` → log only for now
    //   (acknowledged with 200 so Composio doesn't retry). A future
    //   commit can wire a domain event for this and let the auth-
    //   re-link UI react.
    // - anything else → log + 200. Unknown event types should not
    //   force Composio into retry/backoff because retry won't help.
    let envelope: WebhookEnvelopeV3 = match serde_json::from_slice(body.as_ref()) {
        Ok(e) => e,
        Err(parse_err) => {
            tracing::warn!(
                error = %parse_err,
                "[composio-webhook] verified delivery has unparseable body; dropping"
            );
            return (StatusCode::BAD_REQUEST, "body is not a WebhookEnvelopeV3").into_response();
        }
    };

    match envelope.event_type.as_str() {
        "composio.trigger.message" => {
            let trigger_slug = envelope.metadata.trigger_slug.trim();
            if trigger_slug.is_empty() {
                tracing::warn!(
                    event_id = %envelope.id,
                    "[composio-webhook] trigger.message delivery missing metadata.trigger_slug; dropping"
                );
                return (
                    StatusCode::BAD_REQUEST,
                    "metadata.trigger_slug is required for composio.trigger.message",
                )
                    .into_response();
            }
            // Composio sends `metadata.toolkit` when it knows the
            // toolkit; otherwise derive from the slug per the TS SDK's
            // `Triggers.normalizeV3Payload` (split on `_`, take prefix,
            // lowercase to match our existing toolkit slug convention).
            let toolkit = if !envelope.metadata.toolkit.trim().is_empty() {
                envelope.metadata.toolkit.trim().to_lowercase()
            } else {
                trigger_slug.split('_').next().unwrap_or("").to_lowercase()
            };
            tracing::info!(
                toolkit = %toolkit,
                trigger = %trigger_slug,
                event_id = %envelope.id,
                log_id = %envelope.metadata.log_id,
                trigger_id = %envelope.metadata.trigger_id,
                "[composio-webhook] dispatching verified trigger to event bus"
            );
            publish_global(DomainEvent::ComposioTriggerReceived {
                toolkit,
                trigger: trigger_slug.to_string(),
                metadata_id: envelope.id,
                metadata_uuid: envelope.metadata.log_id,
                payload: envelope.data,
            });
            (StatusCode::OK, "ok").into_response()
        }
        "composio.connected_account.expired" => {
            tracing::warn!(
                event_id = %envelope.id,
                "[composio-webhook] connected_account.expired received (no domain event wired yet); acknowledging with 200"
            );
            (StatusCode::OK, "ok").into_response()
        }
        other => {
            tracing::info!(
                event_type = %other,
                event_id = %envelope.id,
                "[composio-webhook] unknown event type — acknowledging with 200 to avoid retry storm"
            );
            (StatusCode::OK, "ok").into_response()
        }
    }
}

/// Bind the receiver to the given loopback port and serve until the
/// returned join handle is dropped or the process exits. Uses
/// `127.0.0.1` exclusively — ngrok provides the public terminus, so
/// binding any other interface would invite external traffic without
/// going through the tunnel's HMAC-checked path.
pub async fn serve(state: ReceiverState, port: u16) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        anyhow::anyhow!(
            "composio webhook receiver: failed to bind {addr}: {e}. \
             Try a different `composio.webhook.local_receiver_port` in config."
        )
    })?;
    let router = build_router(state);
    tracing::info!(
        addr = %addr,
        "[composio-webhook] local receiver listening on loopback (ngrok terminates HTTPS)"
    );
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "[composio-webhook] axum::serve exited");
        }
    });
    Ok(handle)
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
