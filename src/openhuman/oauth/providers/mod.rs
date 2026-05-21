//! Per-provider OAuth 2.0 + PKCE clients. Each provider is a thin module
//! over the same shape:
//!
//!   * an `AUTH_ENDPOINT` / `TOKEN_ENDPOINT` pair,
//!   * a list of scopes the desktop app requests by default,
//!   * a `build_auth_url(&params) -> String` builder,
//!   * `exchange_code(...) -> TokenResponse` and `refresh_token(...) ->
//!     TokenResponse` async functions hitting the token endpoint.
//!
//! Higher-level orchestration (spinning up the loopback, generating PKCE,
//! storing the resulting tokens) lives in [`super::ops`] and is shared
//! across providers.

use thiserror::Error;

pub mod github;
pub mod google;

#[cfg(test)]
mod github_tests;
#[cfg(test)]
mod google_tests;

/// Errors common to every provider's token-endpoint client. Shared so
/// the higher-level orchestrator does not have to switch on a per-
/// provider error type.
#[derive(Debug, Error)]
pub enum TokenError {
    /// The provider returned a non-2xx HTTP status. `body` is the raw
    /// response body verbatim so callers can surface the
    /// `error_description` / `error` payload providers embed in JSON.
    #[error("token endpoint returned HTTP {status}: {body}")]
    Http { status: u16, body: String },

    /// Underlying transport failed (DNS, TLS, connection reset, …).
    #[error("network error talking to token endpoint: {0}")]
    Network(String),

    /// Provider returned 2xx but the body did not parse into the
    /// expected response type. Carries the raw body for debugging.
    #[error("could not decode token response: {message} (body={body})")]
    Decode { message: String, body: String },
}
