// ABOUTME: ngrok tunnel wrapper exposing the local Composio webhook receiver.
// ABOUTME: Free-tier static domain bridges public HTTPS → 127.0.0.1:<port>.

//! # ngrok tunnel
//!
//! Embeds the [ngrok agent SDK](https://github.com/ngrok/ngrok-rust)
//! in-process so the user doesn't need to install a separate CLI.
//! Listens on the static domain assigned to their free ngrok account
//! (`<id>.ngrok-free.dev`) and forwards traffic to the loopback Axum
//! receiver in [`super::server`].
//!
//! Why ngrok over Cloudflare Tunnel: free tier supports ONE persistent
//! static domain per account, which is exactly what we need for a
//! Composio webhook subscription URL that must survive app restarts.
//! Cloudflare quick tunnels (`trycloudflare.com`) are ephemeral; their
//! stable equivalents require a user-owned CF-managed domain.
//!
//! Reconnection: the ngrok SDK manages reconnect to the control plane
//! automatically. If the session dies (e.g. machine sleep/wake) the
//! background `forwarder` task drops its outgoing channel; we surface
//! the failure into [`TunnelState::Error`] and the lifecycle layer can
//! reschedule a `start()` call.

use std::sync::Arc;

use ngrok::prelude::*;
use parking_lot::RwLock;
use tokio::task::JoinHandle;

/// Lifecycle states the tunnel can be in. Used by the Settings →
/// Triggers panel to render the right indicator.
#[derive(Debug, Clone)]
pub enum TunnelState {
    /// No ngrok credentials configured; nothing to do.
    Idle,
    /// Session connect is in flight.
    Connecting,
    /// Tunnel is up; `public_url` is the registered Composio webhook
    /// target.
    Ready { public_url: String },
    /// Session terminated or never started. The string is the last
    /// observed error suitable for surfacing to the user.
    Error(String),
}

impl TunnelState {
    pub fn public_url(&self) -> Option<&str> {
        match self {
            Self::Ready { public_url } => Some(public_url.as_str()),
            _ => None,
        }
    }
}

/// Tunnel handle. Shared via `Arc` between the lifecycle layer (which
/// reads state for the status RPC) and the background forwarder task
/// (which updates state on transitions).
pub struct Tunnel {
    state: Arc<RwLock<TunnelState>>,
    #[allow(dead_code)] // held to keep the task alive; aborted on drop
    forwarder: JoinHandle<()>,
}

impl Tunnel {
    pub fn state(&self) -> TunnelState {
        self.state.read().clone()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // Aborting the forwarder task drops the session, which closes
        // the tunnel on the ngrok side. Matches the "die with the
        // app" lifecycle decision in
        // `~/.claude/plans/fancy-meandering-cake.md`.
        self.forwarder.abort();
    }
}

/// Connect to ngrok with the user's authtoken and forward the static
/// domain to the local loopback receiver on `local_port`.
///
/// Returns a `Tunnel` whose Drop terminates the session. Idle when
/// any required input is missing (returned `Tunnel` is `Idle`); the
/// receiver continues to listen on loopback, just unreachable
/// publicly — Composio retries will surface the missing-receiver
/// state through 503 / network unreachable errors.
///
/// `static_domain` is the bare hostname from the user's ngrok
/// dashboard (e.g. `"abc-123-xyz.ngrok-free.dev"`). We construct
/// `https://<static_domain>` for the public URL surfaced via state.
pub async fn connect(
    authtoken: String,
    static_domain: String,
    local_port: u16,
) -> anyhow::Result<Tunnel> {
    let state = Arc::new(RwLock::new(TunnelState::Connecting));
    let state_for_task = state.clone();

    let forwarder = tokio::spawn(async move {
        match build_session(&authtoken).await {
            Ok(session) => match build_listener(&session, &static_domain, local_port).await {
                Ok(listener) => {
                    let public_url = format!("https://{static_domain}");
                    *state_for_task.write() = TunnelState::Ready {
                        public_url: public_url.clone(),
                    };
                    tracing::info!(
                        public_url = %public_url,
                        local_port,
                        "[composio-webhook] ngrok tunnel ready — forwarding to loopback"
                    );
                    // Hold the listener for its lifetime so the
                    // tunnel stays up. `listener` is a future on
                    // its own join_next branch — await terminates
                    // when the session ends.
                    if let Err(e) = listener.await {
                        let msg = format!("ngrok listener exited: {e:#}");
                        tracing::error!(error = %msg, "[composio-webhook] tunnel terminated");
                        *state_for_task.write() = TunnelState::Error(msg);
                    } else {
                        *state_for_task.write() =
                            TunnelState::Error("ngrok listener closed cleanly".into());
                    }
                }
                Err(e) => {
                    let msg = format!("ngrok listen_and_forward failed: {e:#}");
                    tracing::error!(error = %msg, "[composio-webhook] tunnel listener setup failed");
                    *state_for_task.write() = TunnelState::Error(msg);
                }
            },
            Err(e) => {
                let msg = format!("ngrok session connect failed: {e:#}");
                tracing::error!(error = %msg, "[composio-webhook] tunnel session connect failed");
                *state_for_task.write() = TunnelState::Error(msg);
            }
        }
    });

    Ok(Tunnel { state, forwarder })
}

async fn build_session(authtoken: &str) -> anyhow::Result<ngrok::Session> {
    let session = ngrok::Session::builder()
        .authtoken(authtoken)
        .connect()
        .await?;
    Ok(session)
}

async fn build_listener(
    session: &ngrok::Session,
    static_domain: &str,
    local_port: u16,
) -> anyhow::Result<JoinHandle<Result<(), anyhow::Error>>> {
    let forward_url = format!("http://127.0.0.1:{local_port}")
        .parse::<url::Url>()
        .map_err(|e| anyhow::anyhow!("composio webhook tunnel: bad forward URL: {e}"))?;
    let session_clone = session.clone();
    let domain = static_domain.to_string();
    let handle = tokio::spawn(async move {
        let _listener = session_clone
            .http_endpoint()
            .domain(domain)
            .listen_and_forward(forward_url)
            .await
            .map_err(|e| anyhow::anyhow!("ngrok listen_and_forward error: {e:#}"))?;
        // listen_and_forward returns a Forwarder that runs until the
        // session is dropped. Park here so the task lifetime matches.
        futures::future::pending::<()>().await;
        Ok::<(), anyhow::Error>(())
    });
    Ok(handle)
}

#[cfg(test)]
#[path = "tunnel_test.rs"]
mod tests;
