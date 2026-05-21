//! Tests for [`super::persistence`]. Drive a real `AuthService` against
//! a `tempfile::TempDir` to exercise the full encrypted-on-disk
//! roundtrip, then read profiles back via the public credentials API.

use tempfile::TempDir;

use crate::openhuman::credentials::profiles::AuthProfileKind;
use crate::openhuman::credentials::AuthService;

use super::persistence::{
    github_token_set, google_token_set, save_github_tokens, save_google_tokens, GITHUB_PROVIDER,
    GOOGLE_PROVIDER,
};
use super::providers::{github, google};

fn fresh_service() -> (TempDir, AuthService) {
    let dir = TempDir::new().expect("tempdir");
    // Force-on encryption to mirror the production posture chosen in
    // tasks/todo.md (config.secrets.encrypt = true).
    let service = AuthService::new(dir.path(), true);
    (dir, service)
}

fn google_response(refresh: Option<&str>) -> google::TokenResponse {
    google::TokenResponse {
        access_token: "ya29.access".into(),
        refresh_token: refresh.map(|s| s.to_string()),
        expires_in: 3599,
        scope: "openid email https://www.googleapis.com/auth/gmail.readonly".into(),
        token_type: "Bearer".into(),
        id_token: Some("eyJ.id.token".into()),
    }
}

fn github_response_classic() -> github::TokenResponse {
    github::TokenResponse {
        access_token: "gho_classic".into(),
        scope: "repo read:user".into(),
        token_type: "bearer".into(),
        expires_in: None,
        refresh_token: None,
        refresh_token_expires_in: None,
    }
}

fn github_response_expiring() -> github::TokenResponse {
    github::TokenResponse {
        access_token: "gho_expiring".into(),
        scope: "repo".into(),
        token_type: "bearer".into(),
        expires_in: Some(28800),
        refresh_token: Some("ghr_refresh".into()),
        refresh_token_expires_in: Some(15897600),
    }
}

// ── token-set mapping (pure) ────────────────────────────────────────────

#[test]
fn google_token_set_carries_id_token_and_expiry() {
    let now = chrono::Utc::now();
    let ts = google_token_set(&google_response(Some("1//rt")));
    assert_eq!(ts.access_token, "ya29.access");
    assert_eq!(ts.refresh_token.as_deref(), Some("1//rt"));
    assert_eq!(ts.id_token.as_deref(), Some("eyJ.id.token"));
    assert_eq!(ts.token_type.as_deref(), Some("Bearer"));
    assert_eq!(
        ts.scope.as_deref(),
        Some("openid email https://www.googleapis.com/auth/gmail.readonly")
    );
    let expires_at = ts.expires_at.expect("google always sets expires_in");
    // 3599 seconds from now, within a generous slack window.
    let delta = (expires_at - now).num_seconds();
    assert!(
        (3590..=3610).contains(&delta),
        "expected expires_at ~= now + 3599s, got delta={delta}s"
    );
}

#[test]
fn github_classic_token_set_has_no_expiry_no_refresh() {
    let ts = github_token_set(&github_response_classic());
    assert_eq!(ts.access_token, "gho_classic");
    assert_eq!(ts.refresh_token, None);
    assert_eq!(ts.expires_at, None);
    assert_eq!(ts.id_token, None);
    assert_eq!(ts.token_type.as_deref(), Some("bearer"));
    assert_eq!(ts.scope.as_deref(), Some("repo read:user"));
}

#[test]
fn github_expiring_token_set_has_expiry_and_refresh() {
    let now = chrono::Utc::now();
    let ts = github_token_set(&github_response_expiring());
    assert_eq!(ts.refresh_token.as_deref(), Some("ghr_refresh"));
    let expires_at = ts
        .expires_at
        .expect("expiring github response has expires_in");
    let delta = (expires_at - now).num_seconds();
    assert!(
        (28790..=28810).contains(&delta),
        "expected expires_at ~= now + 28800s, got delta={delta}s"
    );
}

// ── disk roundtrip via AuthService ──────────────────────────────────────

#[test]
fn save_google_tokens_roundtrips_through_authservice() {
    let (_dir, service) = fresh_service();

    let saved = save_google_tokens(&service, "default", &google_response(Some("1//rt"))).unwrap();
    assert_eq!(saved.provider, GOOGLE_PROVIDER);
    assert_eq!(saved.profile_name, "default");
    assert!(matches!(saved.kind, AuthProfileKind::OAuth));

    // Reload through the public API; tokens must survive the encrypted
    // write+read cycle byte-for-byte.
    let loaded = service
        .get_profile(GOOGLE_PROVIDER, None)
        .unwrap()
        .expect("profile should exist after save");
    let ts = loaded
        .token_set
        .expect("google profile must carry a token_set");
    assert_eq!(ts.access_token, "ya29.access");
    assert_eq!(ts.refresh_token.as_deref(), Some("1//rt"));
    assert_eq!(ts.id_token.as_deref(), Some("eyJ.id.token"));
    assert!(ts.expires_at.is_some());
}

#[test]
fn save_github_tokens_roundtrips_through_authservice() {
    let (_dir, service) = fresh_service();
    let _ = save_github_tokens(&service, "default", &github_response_classic()).unwrap();

    let loaded = service
        .get_profile(GITHUB_PROVIDER, None)
        .unwrap()
        .expect("github profile should exist after save");
    let ts = loaded.token_set.unwrap();
    assert_eq!(ts.access_token, "gho_classic");
    assert!(ts.refresh_token.is_none());
    assert!(ts.expires_at.is_none());
}

#[test]
fn save_does_not_cross_contaminate_providers() {
    // Saving Google and GitHub side by side must produce two distinct
    // profiles — a regression here would mean one provider's refresh
    // token could overwrite the other's.
    let (_dir, service) = fresh_service();
    let _ = save_google_tokens(&service, "default", &google_response(Some("g_rt"))).unwrap();
    let _ = save_github_tokens(&service, "default", &github_response_expiring()).unwrap();

    let g = service.get_profile(GOOGLE_PROVIDER, None).unwrap().unwrap();
    let gh = service.get_profile(GITHUB_PROVIDER, None).unwrap().unwrap();
    assert_eq!(g.token_set.unwrap().access_token, "ya29.access");
    assert_eq!(gh.token_set.unwrap().access_token, "gho_expiring");
}

#[test]
fn save_google_tokens_marks_profile_active() {
    // The OAuth flow always activates the profile it just minted so
    // downstream callers' `get_profile(provider, None)` finds it.
    let (_dir, service) = fresh_service();
    let _ = save_google_tokens(&service, "default", &google_response(Some("rt"))).unwrap();

    // get_profile(provider, None) reads the active profile; if save did
    // not set active, this would return None.
    let loaded = service.get_profile(GOOGLE_PROVIDER, None).unwrap();
    assert!(loaded.is_some(), "save must mark the new profile active");
}

#[test]
fn save_google_tokens_idempotent_on_repeat_save() {
    // A re-auth (user runs the flow again) must overwrite the existing
    // profile in place, not create a second one or fail.
    let (_dir, service) = fresh_service();
    let _ = save_google_tokens(&service, "default", &google_response(Some("rt1"))).unwrap();
    let second = save_google_tokens(&service, "default", &google_response(Some("rt2"))).unwrap();

    assert_eq!(second.provider, GOOGLE_PROVIDER);
    let loaded = service.get_profile(GOOGLE_PROVIDER, None).unwrap().unwrap();
    assert_eq!(
        loaded.token_set.unwrap().refresh_token.as_deref(),
        Some("rt2"),
        "second save must replace the first refresh token"
    );

    let all = service.load_profiles().unwrap();
    let google_profiles: Vec<_> = all
        .profiles
        .values()
        .filter(|p| p.provider == GOOGLE_PROVIDER)
        .collect();
    assert_eq!(
        google_profiles.len(),
        1,
        "repeat save must not create a duplicate profile"
    );
}
