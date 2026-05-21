//! Provider-agnostic token-refresh helper. On a 401 from any
//! provider API, the caller can run [`refresh_provider_token`] to
//! trade the stored `refresh_token` for a fresh `access_token` and
//! persist it. The original API call can then be retried once with
//! the new bearer.
//!
//! Centralizing the refresh dispatch here means:
//!   * `bearer::AuthedClient`'s 401 handler does not need to know
//!     anything about provider-specific token endpoints.
//!   * Tests can swap a mock token endpoint per-provider without
//!     plumbing a new param through every caller.
//!
//! Refreshing requires the build-time OAuth client_id (same as the
//! initial flow), so a refresh attempt without
//! `OPENHUMAN_{GOOGLE,GITHUB}_OAUTH_CLIENT_ID` set at build returns
//! [`RefreshError::ClientIdMissing`] — the user has to rebuild with
//! the env var or run the full OAuth flow.

use anyhow::{anyhow, Result};
use thiserror::Error;

use crate::openhuman::credentials::profiles::TokenSet;
use crate::openhuman::credentials::AuthService;

use super::ops::{GITHUB_CLIENT_ID, GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET};
use super::persistence::{github_token_set, google_token_set, GITHUB_PROVIDER, GOOGLE_PROVIDER};
use super::providers::{github, google};

#[derive(Debug, Error)]
pub enum RefreshError {
    #[error(
        "OAuth client for provider '{provider}' is not configured — rebuild with {env_var} set"
    )]
    ClientIdMissing {
        provider: &'static str,
        env_var: &'static str,
    },

    #[error("no profile stored for provider '{provider}' — run the OAuth flow first")]
    NoProfile { provider: String },

    #[error("profile for '{provider}' has no stored refresh_token — re-run the OAuth flow")]
    NoRefreshToken { provider: String },

    #[error("token endpoint refresh failed: {0}")]
    Token(String),

    #[error("persisting refreshed tokens failed: {0}")]
    Persistence(String),
}

/// Trade the stored refresh token for a new access token + scopes.
/// On success, the new `TokenSet` is persisted under the same active
/// profile, with the original refresh_token preserved if the provider
/// did not return a new one (Google never does, GitHub does on
/// expiring-OAuth-App tokens).
///
/// Returns the freshly persisted `TokenSet` so callers that already
/// have the old one in hand can swap without an extra
/// `AuthService::get_profile` call.
pub async fn refresh_provider_token(
    http: &reqwest::Client,
    service: &AuthService,
    provider: &str,
) -> Result<TokenSet, RefreshError> {
    match provider {
        GOOGLE_PROVIDER => refresh_google(http, service).await,
        GITHUB_PROVIDER => refresh_github(http, service).await,
        other => Err(RefreshError::Token(format!(
            "no refresh impl for provider '{other}'"
        ))),
    }
}

async fn refresh_google(
    http: &reqwest::Client,
    service: &AuthService,
) -> Result<TokenSet, RefreshError> {
    let client_id = GOOGLE_CLIENT_ID.ok_or(RefreshError::ClientIdMissing {
        provider: "google",
        env_var: "OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID",
    })?;
    let (existing_refresh, profile_name) = load_refresh_token(service, GOOGLE_PROVIDER)?;
    let mut client = google::GoogleClient::new(http.clone(), client_id);
    if let Some(secret) = GOOGLE_CLIENT_SECRET {
        client = client.with_client_secret(secret);
    }
    let new = client
        .refresh_access_token(&existing_refresh)
        .await
        .map_err(|e| RefreshError::Token(e.to_string()))?;
    // Google never returns a fresh refresh_token on refresh-grant
    // responses, so synthesize a TokenResponse that carries the
    // stored refresh_token forward before mapping to TokenSet.
    let mapped = google::TokenResponse {
        refresh_token: new.refresh_token.clone().or(Some(existing_refresh)),
        ..new
    };
    let ts = google_token_set(&mapped);
    persist(service, GOOGLE_PROVIDER, &profile_name, ts.clone())?;
    Ok(ts)
}

async fn refresh_github(
    http: &reqwest::Client,
    service: &AuthService,
) -> Result<TokenSet, RefreshError> {
    let client_id = GITHUB_CLIENT_ID.ok_or(RefreshError::ClientIdMissing {
        provider: "github",
        env_var: "OPENHUMAN_GITHUB_OAUTH_CLIENT_ID",
    })?;
    let (existing_refresh, profile_name) = load_refresh_token(service, GITHUB_PROVIDER)?;
    let client = github::GithubClient::new(http.clone(), client_id);
    let new = client
        .refresh_access_token(&existing_refresh)
        .await
        .map_err(|e| RefreshError::Token(e.to_string()))?;
    // GitHub OAuth Apps with expiring tokens return a fresh refresh
    // on each refresh; classic OAuth Apps don't. Preserve whichever
    // value survived.
    let mapped = github::TokenResponse {
        refresh_token: new.refresh_token.clone().or(Some(existing_refresh)),
        ..new
    };
    let ts = github_token_set(&mapped);
    persist(service, GITHUB_PROVIDER, &profile_name, ts.clone())?;
    Ok(ts)
}

fn load_refresh_token(
    service: &AuthService,
    provider: &str,
) -> Result<(String, String), RefreshError> {
    let profile = service
        .get_profile(provider, None)
        .map_err(|e| RefreshError::Token(anyhow!(e).to_string()))?
        .ok_or_else(|| RefreshError::NoProfile {
            provider: provider.into(),
        })?;
    let refresh_token = profile
        .token_set
        .and_then(|ts| ts.refresh_token)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RefreshError::NoRefreshToken {
            provider: provider.into(),
        })?;
    Ok((refresh_token, profile.profile_name))
}

fn persist(
    service: &AuthService,
    provider: &str,
    profile_name: &str,
    token_set: TokenSet,
) -> Result<(), RefreshError> {
    service
        .store_provider_oauth_tokens(provider, profile_name, token_set, Default::default(), true)
        .map(|_| ())
        .map_err(|e| RefreshError::Persistence(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_service() -> (TempDir, AuthService) {
        let dir = TempDir::new().unwrap();
        let svc = AuthService::new(dir.path(), true);
        (dir, svc)
    }

    #[tokio::test]
    async fn no_profile_returns_typed_error() {
        let (_d, svc) = fresh_service();
        let http = reqwest::Client::new();
        let err = refresh_provider_token(&http, &svc, GOOGLE_PROVIDER)
            .await
            .unwrap_err();
        // ClientIdMissing wins if GOOGLE_CLIENT_ID is None at build
        // time (the common case in dev); otherwise NoProfile. Either
        // is a clean typed error pointing the user at the right fix.
        match err {
            RefreshError::ClientIdMissing { provider, .. } => assert_eq!(provider, "google"),
            RefreshError::NoProfile { provider } => assert_eq!(provider, GOOGLE_PROVIDER),
            other => panic!("expected ClientIdMissing or NoProfile, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_provider_returns_token_error() {
        let (_d, svc) = fresh_service();
        let http = reqwest::Client::new();
        let err = refresh_provider_token(&http, &svc, "slack")
            .await
            .unwrap_err();
        match err {
            RefreshError::Token(msg) => {
                assert!(msg.contains("no refresh impl"), "msg={msg}");
                assert!(
                    msg.contains("slack"),
                    "msg should name the bad provider: {msg}"
                );
            }
            other => panic!("expected Token(no refresh impl), got {other:?}"),
        }
    }

    #[test]
    fn load_refresh_token_errors_when_no_token_set() {
        let (_d, svc) = fresh_service();
        // Store a TokenSet that intentionally lacks a refresh_token —
        // simulates an OAuth flow that ran but the provider did not
        // return a refresh token (e.g. user revoked offline access).
        let ts = TokenSet {
            access_token: "at".into(),
            refresh_token: None,
            id_token: None,
            expires_at: None,
            token_type: Some("Bearer".into()),
            scope: Some("test".into()),
        };
        svc.store_provider_oauth_tokens(GOOGLE_PROVIDER, "default", ts, Default::default(), true)
            .unwrap();
        let err = load_refresh_token(&svc, GOOGLE_PROVIDER).unwrap_err();
        assert!(matches!(err, RefreshError::NoRefreshToken { .. }));
    }
}
