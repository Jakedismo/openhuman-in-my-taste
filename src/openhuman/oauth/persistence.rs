//! Persistence layer for native-OAuth tokens. Maps provider-specific
//! token-endpoint responses into the existing
//! [`TokenSet`] schema and upserts them
//! through [`crate::openhuman::credentials::AuthService`].
//!
//! No new persistence machinery is introduced — tokens land in the
//! same encrypted-at-rest profile store the rest of the app already
//! uses (`<workspace>/state/<auth-profiles>` when
//! `config.secrets.encrypt` is true). Loading goes through the same
//! `AuthService::get_profile` path.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

use crate::openhuman::credentials::profiles::{AuthProfile, TokenSet};
use crate::openhuman::credentials::AuthService;

use super::providers::{github, google};

/// Provider key used to namespace stored profiles for Google OAuth.
pub const GOOGLE_PROVIDER: &str = "google";
/// Provider key used to namespace stored profiles for GitHub OAuth.
pub const GITHUB_PROVIDER: &str = "github";
/// Profile name used when the caller does not specify a named profile.
pub const DEFAULT_PROFILE_NAME: &str = "default";

/// Helper: turn a relative `expires_in` (seconds) into an absolute
/// `expires_at`. Returns `None` if the provider did not supply one
/// (e.g. classic GitHub OAuth-App tokens that never expire).
fn expires_at_from_relative(seconds: Option<u64>) -> Option<DateTime<Utc>> {
    let seconds = seconds?;
    let delta = Duration::try_seconds(seconds.try_into().ok()?)?;
    Some(Utc::now() + delta)
}

/// Map Google's [`google::TokenResponse`] into the shared
/// [`TokenSet`](TokenSet) schema.
pub fn google_token_set(response: &google::TokenResponse) -> TokenSet {
    TokenSet {
        access_token: response.access_token.clone(),
        refresh_token: response.refresh_token.clone(),
        id_token: response.id_token.clone(),
        // Google always returns `expires_in` (seconds). Convert to an
        // absolute timestamp so refresh-on-expiry logic does not have
        // to track when the token was issued.
        expires_at: expires_at_from_relative(Some(response.expires_in)),
        token_type: Some(response.token_type.clone()),
        scope: Some(response.scope.clone()),
    }
}

/// Map GitHub's [`github::TokenResponse`] into the shared
/// [`TokenSet`](TokenSet) schema. Note
/// that GitHub's `expires_in` is optional — classic OAuth-App tokens
/// have no expiry, so `expires_at` stays `None` in that case.
pub fn github_token_set(response: &github::TokenResponse) -> TokenSet {
    TokenSet {
        access_token: response.access_token.clone(),
        refresh_token: response.refresh_token.clone(),
        id_token: None,
        expires_at: expires_at_from_relative(response.expires_in),
        token_type: Some(response.token_type.clone()),
        scope: Some(response.scope.clone()),
    }
}

/// Save the result of a successful Google PKCE handshake under the
/// `GOOGLE_PROVIDER` namespace. Marks the profile active so subsequent
/// `get_profile` calls without an override pick it up.
pub fn save_google_tokens(
    service: &AuthService,
    profile_name: &str,
    response: &google::TokenResponse,
) -> Result<AuthProfile> {
    service.store_provider_oauth_tokens(
        GOOGLE_PROVIDER,
        profile_name,
        google_token_set(response),
        HashMap::new(),
        true,
    )
}

/// Save the result of a successful GitHub PKCE handshake under the
/// `GITHUB_PROVIDER` namespace.
pub fn save_github_tokens(
    service: &AuthService,
    profile_name: &str,
    response: &github::TokenResponse,
) -> Result<AuthProfile> {
    service.store_provider_oauth_tokens(
        GITHUB_PROVIDER,
        profile_name,
        github_token_set(response),
        HashMap::new(),
        true,
    )
}
