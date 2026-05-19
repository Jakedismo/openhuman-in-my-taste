//! Tests for [`super::google`]. Focus is on parameter pinning — every
//! key in [`build_auth_url`] is required to land exactly once, with the
//! exact value documented in the rustdoc. A silent regression on any
//! of them (e.g. losing `prompt=consent`) would silently break the
//! refresh-token recovery story without breaking any compile-time check.

use std::collections::HashMap;

use url::Url;

use super::google::{build_auth_url, AuthUrlParams, AUTH_ENDPOINT, DEFAULT_SCOPES};

fn parse(url: &str) -> (String, HashMap<String, String>) {
    let parsed = Url::parse(url).expect("auth url must be a valid URL");
    let base = format!(
        "{}://{}{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or(""),
        parsed.path()
    );
    let params = parsed.query_pairs().into_owned().collect();
    (base, params)
}

fn sample_params<'a>() -> AuthUrlParams<'a> {
    AuthUrlParams {
        client_id: "12345.apps.googleusercontent.com",
        redirect_uri: "http://127.0.0.1:54321/oauth/callback",
        scopes: DEFAULT_SCOPES,
        state: "csrf_state_abc",
        code_challenge: "EXAMPLE_CHALLENGE",
    }
}

#[test]
fn auth_url_points_at_google_authorize_endpoint() {
    let p = sample_params();
    let (base, _) = parse(&build_auth_url(&p));
    assert_eq!(base, AUTH_ENDPOINT);
}

#[test]
fn auth_url_carries_all_required_keys_exactly_once() {
    let p = sample_params();
    let (_, params) = parse(&build_auth_url(&p));
    for key in [
        "client_id",
        "redirect_uri",
        "response_type",
        "scope",
        "state",
        "code_challenge",
        "code_challenge_method",
        "access_type",
        "prompt",
        "include_granted_scopes",
    ] {
        assert!(
            params.contains_key(key),
            "auth URL missing required key '{key}': {params:?}"
        );
    }
}

#[test]
fn auth_url_pins_fixed_values() {
    let p = sample_params();
    let (_, params) = parse(&build_auth_url(&p));
    assert_eq!(
        params.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        params.get("access_type").map(String::as_str),
        Some("offline")
    );
    assert_eq!(params.get("prompt").map(String::as_str), Some("consent"));
    assert_eq!(
        params.get("include_granted_scopes").map(String::as_str),
        Some("true")
    );
}

#[test]
fn auth_url_passes_caller_values_through() {
    let p = sample_params();
    let (_, params) = parse(&build_auth_url(&p));
    assert_eq!(params.get("client_id").unwrap(), p.client_id);
    assert_eq!(params.get("redirect_uri").unwrap(), p.redirect_uri);
    assert_eq!(params.get("state").unwrap(), p.state);
    assert_eq!(params.get("code_challenge").unwrap(), p.code_challenge);
}

#[test]
fn auth_url_joins_scopes_with_spaces() {
    let p = sample_params();
    let (_, params) = parse(&build_auth_url(&p));
    let expected = DEFAULT_SCOPES.join(" ");
    // `url::Url::query_pairs` already percent-decodes — so spaces here
    // mean the raw form was either `+` or `%20`, which both decode to
    // a space. Either is acceptable wire-format for Google.
    assert_eq!(params.get("scope").unwrap(), &expected);
}

#[test]
fn auth_url_percent_encodes_redirect_uri_in_wire_form() {
    let p = sample_params();
    let raw = build_auth_url(&p);
    // The raw redirect_uri contains `:` and `/`. The wire-format query
    // string MUST percent-encode them, otherwise providers reject the
    // request with `redirect_uri_mismatch`.
    assert!(
        raw.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A54321%2Foauth%2Fcallback"),
        "redirect_uri must be percent-encoded in the raw URL: {raw}"
    );
}

#[test]
fn auth_url_accepts_custom_scope_list() {
    // Callers can request a narrower scope set, e.g. for re-auth flows
    // that only need a single capability.
    let scopes: &[&str] = &["openid", "email"];
    let p = AuthUrlParams {
        scopes,
        ..sample_params()
    };
    let (_, params) = parse(&build_auth_url(&p));
    assert_eq!(params.get("scope").unwrap(), "openid email");
}

#[test]
fn default_scopes_include_the_v1_provider_surface() {
    // Guard against an accidental scope-list trim. If someone removes
    // gmail/calendar/drive scopes the tests should scream.
    for needed in [
        "https://www.googleapis.com/auth/gmail.readonly",
        "https://www.googleapis.com/auth/gmail.send",
        "https://www.googleapis.com/auth/calendar",
        "https://www.googleapis.com/auth/drive.file",
    ] {
        assert!(
            DEFAULT_SCOPES.contains(&needed),
            "DEFAULT_SCOPES is missing required scope '{needed}'"
        );
    }
}
