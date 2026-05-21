//! One-shot loopback redirect server for native OAuth 2.0 PKCE flows.
//!
//! RFC 8252 §7.3 ("loopback IP redirection") describes the recommended
//! redirect strategy for native apps: bind an ephemeral port on
//! `127.0.0.1`, advertise `http://127.0.0.1:<port>/oauth/callback` as the
//! redirect URI, open the system browser at the provider's authorization
//! endpoint, and wait for the provider's redirect to drop the
//! `code` + `state` parameters back to us on that local port.
//!
//! Lifecycle:
//!   1. [`spawn_loopback`] binds `127.0.0.1:0`, returns a [`LoopbackHandle`]
//!      carrying the bound `redirect_uri` and a callback receiver.
//!   2. The caller opens `auth_url` (built with that `redirect_uri`) in the
//!      user's browser.
//!   3. The provider redirects the browser to `redirect_uri?code=…&state=…`
//!      (or `?error=…` on denial). The handler validates required params,
//!      delivers a [`CallbackParams`] (or [`OAuthCallbackError`]) to the
//!      caller, and returns a small HTML landing page.
//!   4. The server shuts down after the first request — it is single-use.
//!
//! The handler is deliberately permissive about the request path: any path
//! is accepted, since providers occasionally append fragments or unusual
//! query layouts. CSRF defense is delegated to the caller (verify the
//! returned `state` matches what was sent).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    response::Html,
    routing::get,
    Router,
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

/// HTML body returned to the browser after a successful (or failed)
/// callback. Short, brand-neutral, no external resources.
const LANDING_PAGE_HTML: &str = "<!DOCTYPE html>\n\
    <html lang=\"en\"><head><meta charset=\"utf-8\">\
    <title>Authorization complete</title></head>\
    <body style=\"font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; \
    padding: 2rem; text-align: center;\">\
    <h1>Authorization complete</h1>\
    <p>You can close this window and return to the app.</p>\
    </body></html>";

/// Parameters parsed from a successful OAuth provider redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackParams {
    pub code: String,
    pub state: String,
}

/// Errors surfaced by the loopback callback receiver.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OAuthCallbackError {
    /// The provider redirected with `?error=…` instead of a code.
    /// `description` is the optional `error_description` URL parameter.
    #[error("provider returned error '{error}'{}", description.as_ref().map(|d| format!(": {d}")).unwrap_or_default())]
    ProviderError {
        error: String,
        description: Option<String>,
    },

    /// The callback URL was missing a required parameter.
    #[error("callback missing required parameter '{0}'")]
    MissingParam(&'static str),

    /// The caller's `await_callback` timeout elapsed before any redirect
    /// reached the server.
    #[error("loopback callback timed out after {0:?}")]
    Timeout(Duration),

    /// Internal error inside the loopback server (channel dropped, task
    /// panicked, etc.).
    #[error("loopback server error: {0}")]
    Server(String),
}

/// Handle returned by [`spawn_loopback`]. Carries the bound redirect URI
/// plus a one-shot channel that resolves to the callback params (or an
/// error) the first time the provider redirects back to the loopback.
pub struct LoopbackHandle {
    pub redirect_uri: String,
    pub port: u16,
    callback_rx: oneshot::Receiver<Result<CallbackParams, OAuthCallbackError>>,
    server_task: JoinHandle<()>,
}

impl LoopbackHandle {
    /// Wait for the provider's redirect, up to `timeout`. Consumes the
    /// handle — the loopback server is single-use by design.
    pub async fn await_callback(
        self,
        timeout: Duration,
    ) -> Result<CallbackParams, OAuthCallbackError> {
        let result = match tokio::time::timeout(timeout, self.callback_rx).await {
            Ok(Ok(inner)) => inner,
            Ok(Err(_)) => Err(OAuthCallbackError::Server(
                "callback channel dropped before redirect".into(),
            )),
            Err(_) => Err(OAuthCallbackError::Timeout(timeout)),
        };
        // The handler triggers server shutdown after delivery, but if the
        // user closed the browser without ever hitting the loopback, the
        // serve task is still alive. Cancel it so we don't leak.
        self.server_task.abort();
        result
    }
}

/// State shared between the axum handler and the caller's oneshot.
/// `Mutex<Option<…>>` lets the first redirect consume the sender; later
/// requests find it gone and return a generic "already used" response.
struct LoopbackState {
    sender: Mutex<Option<oneshot::Sender<Result<CallbackParams, OAuthCallbackError>>>>,
}

/// Bind an ephemeral port on `127.0.0.1`, spawn a one-shot axum server,
/// and return a [`LoopbackHandle`] the caller can await for the redirect.
pub async fn spawn_loopback() -> anyhow::Result<LoopbackHandle> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");

    let (tx, rx) = oneshot::channel();
    let state = Arc::new(LoopbackState {
        sender: Mutex::new(Some(tx)),
    });

    let app = Router::new()
        .fallback(get(callback_handler))
        .with_state(state);

    let server_task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!("[oauth.loopback] axum::serve returned with error: {e}");
        }
    });

    Ok(LoopbackHandle {
        redirect_uri,
        port,
        callback_rx: rx,
        server_task,
    })
}

/// Axum handler. Parses query parameters out of the URI (regardless of
/// path) and delivers a [`CallbackParams`] or [`OAuthCallbackError`] to
/// the caller's one-shot. Returns the static landing page either way.
async fn callback_handler(
    State(state): State<Arc<LoopbackState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Html<&'static str> {
    let result = parse_callback(&params);

    // Take the sender out of the option; if it's already gone (duplicate
    // redirect, retried request), we simply respond with the landing page
    // and let the original delivery stand.
    let mut sender_guard = state.sender.lock().await;
    if let Some(sender) = sender_guard.take() {
        let _ = sender.send(result);
    }
    Html(LANDING_PAGE_HTML)
}

/// Pure-function logic split out for testability. Maps raw query params
/// to either a [`CallbackParams`] or a tagged [`OAuthCallbackError`].
pub(super) fn parse_callback(
    params: &HashMap<String, String>,
) -> Result<CallbackParams, OAuthCallbackError> {
    if let Some(error) = params.get("error") {
        return Err(OAuthCallbackError::ProviderError {
            error: error.clone(),
            description: params.get("error_description").cloned(),
        });
    }
    let code = params
        .get("code")
        .cloned()
        .ok_or(OAuthCallbackError::MissingParam("code"))?;
    let state = params
        .get("state")
        .cloned()
        .ok_or(OAuthCallbackError::MissingParam("state"))?;
    Ok(CallbackParams { code, state })
}
