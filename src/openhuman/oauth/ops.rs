//! High-level orchestration for native OAuth flows. Composes:
//!
//!   * [`super::pkce`]               — verifier / challenge / state
//!   * [`super::loopback`]           — one-shot 127.0.0.1 redirect server
//!   * [`super::providers::google`]  /
//!     [`super::providers::github`]  — auth-URL builder + token client
//!   * [`super::persistence`]        — save into `AuthService`
//!
//! The intended caller-shape is:
//!
//! ```ignore
//! let http = reqwest::Client::new();
//! let flow = start_google_flow(http.clone()).await?;
//! open_in_browser(&flow.auth_url);
//! let completion = flow.complete(&auth_service, "default", TIMEOUT).await?;
//! ```
//!
//! Client IDs are baked at build time via the
//! `OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID` and
//! `OPENHUMAN_GITHUB_OAUTH_CLIENT_ID` env vars. Unset at build time →
//! the matching `start_*_flow` returns
//! [`OAuthFlowError::ClientIdMissing`] at runtime with the env var name
//! so the user knows what to set.

use std::time::Duration;

use thiserror::Error;

use crate::openhuman::credentials::profiles::AuthProfile;
use crate::openhuman::credentials::AuthService;

use super::loopback::{spawn_loopback, LoopbackHandle, OAuthCallbackError};
use super::persistence::{save_github_tokens, save_google_tokens};
use super::pkce;
use super::providers::{github, google, TokenError};

/// Compile-time-baked Google OAuth client ID. Set the env var
/// `OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID` when building the binary. Unset
/// means the desktop app cannot start a Google flow; the user will see
/// [`OAuthFlowError::ClientIdMissing`] at runtime.
pub const GOOGLE_CLIENT_ID: Option<&str> = option_env!("OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID");

/// Compile-time-baked Google OAuth client_secret. Google requires
/// this in the token-exchange and refresh requests even for desktop
/// (installed) OAuth clients. Its docs explicitly allow shipping the
/// secret in installed-app binaries since it cannot truly be kept
/// confidential there. Set `OPENHUMAN_GOOGLE_OAUTH_CLIENT_SECRET`
/// at build time alongside the client ID. Unset is allowed but the
/// token endpoint will reject the exchange with
/// `client_secret is missing`.
pub const GOOGLE_CLIENT_SECRET: Option<&str> = option_env!("OPENHUMAN_GOOGLE_OAUTH_CLIENT_SECRET");

/// Compile-time-baked GitHub OAuth client ID. Same shape as
/// [`GOOGLE_CLIENT_ID`] — set `OPENHUMAN_GITHUB_OAUTH_CLIENT_ID` at
/// build time or accept the runtime error.
pub const GITHUB_CLIENT_ID: Option<&str> = option_env!("OPENHUMAN_GITHUB_OAUTH_CLIENT_ID");

/// Errors surfaced by the orchestrator.
#[derive(Debug, Error)]
pub enum OAuthFlowError {
    /// Build-time OAuth client ID env var was not set. Tells the user
    /// exactly which env var to set.
    #[error(
        "OAuth client for provider '{provider}' is not configured — set {env_var} at build time"
    )]
    ClientIdMissing {
        provider: &'static str,
        env_var: &'static str,
    },

    /// Loopback server failed to bind or accept.
    #[error("loopback redirect server failed: {0}")]
    Loopback(String),

    /// Provider's redirect arrived with `?error=…` or invalid params.
    #[error("provider callback failed: {0}")]
    Callback(#[from] OAuthCallbackError),

    /// The `state` value returned by the provider's redirect did not
    /// match what we sent — refuse to proceed, since this is the CSRF
    /// boundary that protects the loopback from injection.
    #[error("state mismatch — refusing to exchange code (possible CSRF attack)")]
    StateMismatch,

    /// Token-exchange or refresh failed at the provider.
    #[error("token endpoint: {0}")]
    Token(#[from] TokenError),

    /// Could not write the token into the encrypted profile store.
    #[error("persisting tokens failed: {0}")]
    Persistence(String),
}

/// Provider tag carried through the flow so [`OAuthFlow::complete`]
/// knows which provider client and persistence function to call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowKind {
    Google,
    Github,
}

/// In-flight OAuth flow. Hold this alive while the user authorizes in
/// their browser — the loopback's TCP listener stays bound for the
/// lifetime of this value. Call [`OAuthFlow::complete`] to drive the
/// rest of the flow once the user has clicked "Allow".
pub struct OAuthFlow {
    /// URL to open in the user's system browser.
    pub auth_url: String,
    /// `http://127.0.0.1:<port>/oauth/callback` — exposed so callers
    /// (or tests) can drive a synthetic redirect.
    pub redirect_uri: String,
    kind: FlowKind,
    state: String,
    verifier: String,
    loopback: LoopbackHandle,
    client_id: String,
    /// Google-only: paired with the client_id at build time.
    /// `None` for GitHub or when unset at build.
    client_secret: Option<String>,
    http: reqwest::Client,
    /// `None` in production (uses the provider's hard-coded token
    /// endpoint); test harnesses can point this at a local axum mock.
    token_endpoint_override: Option<String>,
}

/// Result of a successful flow.
#[derive(Debug)]
pub struct OAuthCompletion {
    pub provider: String,
    pub profile: AuthProfile,
}

impl OAuthFlow {
    /// Test-only override of the token endpoint. Threads through to the
    /// provider client's `with_token_endpoint`. Production callers do
    /// NOT have access to this.
    #[cfg(test)]
    pub(crate) fn with_token_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.token_endpoint_override = Some(endpoint.into());
        self
    }

    /// Wait up to `timeout` for the provider's redirect, then exchange
    /// the returned code for tokens and persist them. Consumes the
    /// flow — the loopback can only fire once.
    pub async fn complete(
        self,
        service: &AuthService,
        profile_name: &str,
        timeout: Duration,
    ) -> Result<OAuthCompletion, OAuthFlowError> {
        let params = self.loopback.await_callback(timeout).await?;
        if params.state != self.state {
            return Err(OAuthFlowError::StateMismatch);
        }
        match self.kind {
            FlowKind::Google => {
                let mut client = google::GoogleClient::new(self.http, &self.client_id);
                if let Some(secret) = self.client_secret.as_deref() {
                    client = client.with_client_secret(secret);
                }
                if let Some(endpoint) = self.token_endpoint_override {
                    client = client.with_token_endpoint(endpoint);
                }
                let resp = client
                    .exchange_code(&self.redirect_uri, &params.code, &self.verifier)
                    .await?;
                let profile = save_google_tokens(service, profile_name, &resp)
                    .map_err(|e| OAuthFlowError::Persistence(e.to_string()))?;
                Ok(OAuthCompletion {
                    provider: super::persistence::GOOGLE_PROVIDER.into(),
                    profile,
                })
            }
            FlowKind::Github => {
                let mut client = github::GithubClient::new(self.http, &self.client_id);
                if let Some(endpoint) = self.token_endpoint_override {
                    client = client.with_token_endpoint(endpoint);
                }
                let resp = client
                    .exchange_code(&self.redirect_uri, &params.code, &self.verifier)
                    .await?;
                let profile = save_github_tokens(service, profile_name, &resp)
                    .map_err(|e| OAuthFlowError::Persistence(e.to_string()))?;
                Ok(OAuthCompletion {
                    provider: super::persistence::GITHUB_PROVIDER.into(),
                    profile,
                })
            }
        }
    }
}

/// Begin a Google OAuth PKCE flow. Resolves the client ID from
/// [`GOOGLE_CLIENT_ID`] (set at build time).
pub async fn start_google_flow(http: reqwest::Client) -> Result<OAuthFlow, OAuthFlowError> {
    let client_id = GOOGLE_CLIENT_ID.ok_or(OAuthFlowError::ClientIdMissing {
        provider: "google",
        env_var: "OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID",
    })?;
    start_google_flow_with(http, client_id, GOOGLE_CLIENT_SECRET).await
}

/// Begin a Google OAuth PKCE flow with a caller-supplied client ID.
/// Hidden from production call sites; used by tests and as the
/// implementation backing [`start_google_flow`].
#[doc(hidden)]
pub async fn start_google_flow_with(
    http: reqwest::Client,
    client_id: &str,
    client_secret: Option<&str>,
) -> Result<OAuthFlow, OAuthFlowError> {
    let verifier = pkce::code_verifier();
    let challenge = pkce::code_challenge(&verifier);
    let state = pkce::state();
    let loopback = spawn_loopback()
        .await
        .map_err(|e| OAuthFlowError::Loopback(e.to_string()))?;
    let redirect_uri = loopback.redirect_uri.clone();
    let auth_url = google::build_auth_url(&google::AuthUrlParams {
        client_id,
        redirect_uri: &redirect_uri,
        scopes: google::DEFAULT_SCOPES,
        state: &state,
        code_challenge: &challenge,
    });
    Ok(OAuthFlow {
        auth_url,
        redirect_uri,
        kind: FlowKind::Google,
        state,
        verifier,
        loopback,
        client_id: client_id.to_string(),
        client_secret: client_secret.map(str::to_owned),
        http,
        token_endpoint_override: None,
    })
}

/// Begin a GitHub OAuth PKCE flow.
pub async fn start_github_flow(http: reqwest::Client) -> Result<OAuthFlow, OAuthFlowError> {
    let client_id = GITHUB_CLIENT_ID.ok_or(OAuthFlowError::ClientIdMissing {
        provider: "github",
        env_var: "OPENHUMAN_GITHUB_OAUTH_CLIENT_ID",
    })?;
    start_github_flow_with(http, client_id).await
}

/// Begin a GitHub OAuth PKCE flow with a caller-supplied client ID.
#[doc(hidden)]
pub async fn start_github_flow_with(
    http: reqwest::Client,
    client_id: &str,
) -> Result<OAuthFlow, OAuthFlowError> {
    let verifier = pkce::code_verifier();
    let challenge = pkce::code_challenge(&verifier);
    let state = pkce::state();
    let loopback = spawn_loopback()
        .await
        .map_err(|e| OAuthFlowError::Loopback(e.to_string()))?;
    let redirect_uri = loopback.redirect_uri.clone();
    let auth_url = github::build_auth_url(&github::AuthUrlParams {
        client_id,
        redirect_uri: &redirect_uri,
        scopes: github::DEFAULT_SCOPES,
        state: &state,
        code_challenge: &challenge,
    });
    Ok(OAuthFlow {
        auth_url,
        redirect_uri,
        kind: FlowKind::Github,
        state,
        verifier,
        loopback,
        client_id: client_id.to_string(),
        client_secret: None,
        http,
        token_endpoint_override: None,
    })
}
