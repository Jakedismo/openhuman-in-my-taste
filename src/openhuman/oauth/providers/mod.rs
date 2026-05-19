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

pub mod google;

#[cfg(test)]
mod google_tests;
