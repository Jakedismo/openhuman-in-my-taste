//! Direct provider API clients — Gmail, Google Calendar, Google Drive,
//! GitHub. Replace the Composio-via-backend proxy with calls that go
//! straight from the desktop app to the provider, using the access
//! tokens that landed in `AuthService` from
//! [`crate::openhuman::oauth`].
//!
//! Each provider module is intentionally narrow: it exposes typed Rust
//! functions for the operations currently used in production (the slugs
//! enumerated in `tasks/phase-1-inventory.md` Phase 6 + the curated
//! catalogs that already shipped). Adding a new operation is a small,
//! additive change — no Composio JSON schemas to keep in sync.
//!
//! Authorization model: every call pulls the active access token from
//! `AuthService` for its provider, attaches it as a Bearer header, and
//! forwards the response. Refresh-on-401 is intentionally not built in
//! here yet — the orchestrator (`oauth::ops`) is the canonical entry
//! point for re-auth flows, and a periodic refresh task can land in a
//! later slice.

use anyhow::{anyhow, Context, Result};

use crate::openhuman::credentials::AuthService;

pub mod bearer;
pub mod github;
pub mod google;

#[cfg(test)]
mod bearer_tests;

/// Helper: pull the currently active access token for `provider` out
/// of `AuthService`. Errors if no profile exists, no token_set is
/// attached, or the access_token field is empty.
pub(crate) fn load_access_token(service: &AuthService, provider: &str) -> Result<String> {
    let profile = service
        .get_profile(provider, None)
        .with_context(|| format!("loading profile for provider '{provider}'"))?
        .ok_or_else(|| {
            anyhow!("no connected account for provider '{provider}' — run the OAuth flow first")
        })?;
    let token = profile
        .token_set
        .ok_or_else(|| anyhow!("profile for '{provider}' has no token_set"))?
        .access_token;
    if token.is_empty() {
        return Err(anyhow!(
            "profile for '{provider}' carries an empty access_token"
        ));
    }
    Ok(token)
}
