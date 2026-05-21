// ABOUTME: Local webhook receiver for Composio direct-mode trigger deliveries.
// ABOUTME: ngrok tunnel + Axum listener + HMAC verify + bus dispatch.

//! # Local Composio webhook receiver
//!
//! When OpenHuman runs in direct mode (`config.composio.mode = "direct"`,
//! no backend session), Composio's trigger deliveries have nowhere to
//! land. This module fills that gap end-to-end:
//!
//! 1. [`tunnel`] exposes a stable public URL via ngrok's free static
//!    domain (`<id>.ngrok-free.dev`), forwarding to a loopback Axum
//!    server.
//! 2. [`server`] verifies the inbound HMAC signature (Svix-style — see
//!    [`hmac`]), parses the body into
//!    [`crate::openhuman::composio::types::ComposioTriggerEvent`], and
//!    publishes `DomainEvent::ComposioTriggerReceived` to the global
//!    event bus.
//! 3. [`subscription`] registers the receiver URL with Composio's
//!    `/api/v3/webhook_subscriptions` so events actually flow.
//!
//! From there, `trigger_triage` / `trigger_reactor`
//! (`src/openhuman/agent/triage/`) handle the event the same way they
//! handled it under the backend-relayed path — receiver is a drop-in
//! source.
//!
//! See `tasks/todo.md` → "Composio direct-mode triggers (Option D)"
//! and the plan at
//! `~/.claude/plans/fancy-meandering-cake.md` for the architecture
//! decisions and out-of-scope items.

pub mod hmac;
pub mod server;
pub mod subscription;
pub mod tunnel;

pub use hmac::{verify, VerifyError, DEFAULT_TIMESTAMP_TOLERANCE_SECS};
pub use server::{build_router, serve, ReceiverState};
pub use subscription::{ensure_subscription, EnsureOutcome, ResolvedSubscription};
pub use tunnel::{connect as connect_tunnel, Tunnel, TunnelState};

// ── Lifecycle façade ────────────────────────────────────────────────

use std::sync::Arc;

use once_cell::sync::OnceCell;
use parking_lot::RwLock;

use crate::openhuman::config::Config;
use crate::openhuman::credentials::ops::get_ngrok_authtoken;

/// Cached webhook URL exposed by [`public_webhook_url`] for the
/// trigger op layer so it can include the URL in the
/// `composio_local_webhook_status` payload AND pass it to
/// [`ensure_subscription`] on the first `enable_trigger` call.
///
/// Updated when [`init`] transitions the tunnel to `Ready`. `None`
/// when the receiver is not running.
static PUBLIC_WEBHOOK_URL: OnceCell<RwLock<Option<String>>> = OnceCell::new();

/// Singleton holder for the active `Tunnel`. Dropping the `Tunnel`
/// terminates the ngrok session, so we keep it pinned for the
/// process lifetime.
static ACTIVE_TUNNEL: OnceCell<RwLock<Option<Tunnel>>> = OnceCell::new();

/// Singleton holder for the local Axum receiver join handle. Kept so
/// the server task isn't dropped on the spawning function returning.
static ACTIVE_SERVER: OnceCell<RwLock<Option<tokio::task::JoinHandle<()>>>> = OnceCell::new();

fn url_slot() -> &'static RwLock<Option<String>> {
    PUBLIC_WEBHOOK_URL.get_or_init(|| RwLock::new(None))
}

fn tunnel_slot() -> &'static RwLock<Option<Tunnel>> {
    ACTIVE_TUNNEL.get_or_init(|| RwLock::new(None))
}

fn server_slot() -> &'static RwLock<Option<tokio::task::JoinHandle<()>>> {
    ACTIVE_SERVER.get_or_init(|| RwLock::new(None))
}

/// Public URL currently exposed by the receiver, if the tunnel is up.
/// Used by [`crate::openhuman::composio::ops`] to decide whether
/// trigger writes can proceed in direct mode and what URL to send to
/// Composio at subscription time.
pub fn public_webhook_url() -> Option<String> {
    url_slot().read().clone()
}

/// Live tunnel state for the status RPC.
pub fn tunnel_state() -> TunnelState {
    tunnel_slot()
        .read()
        .as_ref()
        .map(|t| t.state())
        .unwrap_or(TunnelState::Idle)
}

/// Start the local receiver and tunnel if the config + credentials
/// admit it. Idempotent: a second call while a tunnel is already up
/// is a no-op (logs a debug line). Errors during connect are surfaced
/// into [`tunnel_state`] as [`TunnelState::Error`] rather than
/// bubbled up — the app continues to run, just without trigger
/// delivery.
///
/// Gating logic (all must hold for the receiver to come online):
///
/// 1. `config.composio.webhook.local_receiver_enabled` is true.
/// 2. `config.composio.webhook.ngrok_domain` is non-empty.
/// 3. An authtoken is stored under `NGROK_AUTHTOKEN_PROVIDER` in
///    `AuthService`.
///
/// Any of those missing → the receiver stays idle; the op layer's
/// trigger writes surface the existing gate error pointing the user
/// to Settings → Triggers.
pub async fn init(config: &Arc<Config>) -> anyhow::Result<()> {
    if !config.composio.webhook.local_receiver_enabled {
        tracing::debug!("[composio-webhook] init: local_receiver_enabled = false; receiver idle");
        return Ok(());
    }
    let domain = config.composio.webhook.ngrok_domain.trim().to_string();
    if domain.is_empty() {
        tracing::warn!(
            "[composio-webhook] init: local_receiver_enabled but ngrok_domain is empty; receiver idle"
        );
        return Ok(());
    }
    let authtoken = match get_ngrok_authtoken(config) {
        Ok(Some(t)) => t,
        Ok(None) => {
            tracing::warn!(
                "[composio-webhook] init: local_receiver_enabled but no ngrok authtoken stored; receiver idle"
            );
            return Ok(());
        }
        Err(e) => {
            tracing::error!(error = %e, "[composio-webhook] init: failed to read ngrok authtoken");
            return Ok(());
        }
    };

    if tunnel_slot().read().is_some() {
        tracing::debug!("[composio-webhook] init: tunnel already active; skipping");
        return Ok(());
    }

    let port = config.composio.webhook.local_receiver_port;
    let state = ReceiverState {
        config: config.clone(),
    };
    let server_handle = match serve(state, port).await {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "[composio-webhook] init: receiver bind failed");
            return Ok(());
        }
    };
    *server_slot().write() = Some(server_handle);

    let tunnel = match connect_tunnel(authtoken, domain.clone(), port).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "[composio-webhook] init: ngrok session connect failed");
            return Ok(());
        }
    };

    *url_slot().write() = Some(format!("https://{domain}/webhook"));
    *tunnel_slot().write() = Some(tunnel);

    tracing::info!(
        public_url = %format!("https://{domain}/webhook"),
        local_port = port,
        "[composio-webhook] init: receiver + tunnel started"
    );
    Ok(())
}

/// Stop the receiver and drop the tunnel. Idempotent: safe to call
/// when the receiver is already idle. Used by the
/// `composio_local_webhook_stop` op and as part of clean shutdown.
pub fn stop() {
    if let Some(handle) = server_slot().write().take() {
        handle.abort();
        tracing::info!("[composio-webhook] stop: receiver task aborted");
    }
    if tunnel_slot().write().take().is_some() {
        tracing::info!("[composio-webhook] stop: tunnel dropped");
    }
    *url_slot().write() = None;
}
