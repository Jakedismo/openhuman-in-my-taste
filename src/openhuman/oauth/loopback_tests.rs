//! Tests for the one-shot loopback redirect server in [`super::loopback`].
//!
//! The pure-function parser ([`parse_callback`]) is exercised
//! directly with synthetic query maps so we can pin every branch without
//! standing up a TCP listener. The end-to-end tests then drive the real
//! axum server over the wire with `reqwest`, which is the same path a
//! browser would take.

use std::collections::HashMap;
use std::time::Duration;

use super::loopback::{parse_callback, spawn_loopback, CallbackParams, OAuthCallbackError};

// ── parse_callback (pure) ───────────────────────────────────────────────

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn parse_extracts_code_and_state() {
    let got = parse_callback(&map(&[("code", "abc"), ("state", "xyz")])).unwrap();
    assert_eq!(
        got,
        CallbackParams {
            code: "abc".into(),
            state: "xyz".into()
        }
    );
}

#[test]
fn parse_missing_code_is_typed_error() {
    let err = parse_callback(&map(&[("state", "xyz")])).unwrap_err();
    assert_eq!(err, OAuthCallbackError::MissingParam("code"));
}

#[test]
fn parse_missing_state_is_typed_error() {
    // `state` is treated as required even though OAuth 2.0 itself lists
    // it as RECOMMENDED. We require it for CSRF defense; a provider that
    // strips it would otherwise let an attacker race our flow.
    let err = parse_callback(&map(&[("code", "abc")])).unwrap_err();
    assert_eq!(err, OAuthCallbackError::MissingParam("state"));
}

#[test]
fn parse_provider_error_with_description() {
    let err = parse_callback(&map(&[
        ("error", "access_denied"),
        ("error_description", "user said no"),
    ]))
    .unwrap_err();
    assert_eq!(
        err,
        OAuthCallbackError::ProviderError {
            error: "access_denied".into(),
            description: Some("user said no".into()),
        }
    );
}

#[test]
fn parse_provider_error_without_description() {
    // Some providers omit `error_description`. We must still flag the error
    // cleanly rather than mistakenly demanding a `code`.
    let err = parse_callback(&map(&[("error", "server_error")])).unwrap_err();
    assert_eq!(
        err,
        OAuthCallbackError::ProviderError {
            error: "server_error".into(),
            description: None,
        }
    );
}

#[test]
fn parse_provider_error_takes_precedence_over_missing_code() {
    // Branch ordering check: if the provider gave us an explicit error,
    // we never go on to complain about a missing code — that would mask
    // the real failure for the caller.
    let err = parse_callback(&map(&[("error", "access_denied")])).unwrap_err();
    matches!(err, OAuthCallbackError::ProviderError { .. });
}

// ── end-to-end over the wire ────────────────────────────────────────────

const NETWORK_TIMEOUT: Duration = Duration::from_secs(3);

async fn drive_callback(redirect_uri: &str, query: &str) {
    // Use rustls-only reqwest so this test does not depend on system TLS.
    let client = reqwest::Client::builder()
        .timeout(NETWORK_TIMEOUT)
        .build()
        .expect("reqwest client");
    let url = format!("{redirect_uri}?{query}");
    let resp = client
        .get(&url)
        .send()
        .await
        .expect("loopback should accept GET");
    assert!(
        resp.status().is_success(),
        "loopback returned {} for {url}",
        resp.status()
    );
}

#[tokio::test]
async fn end_to_end_delivers_code_and_state() {
    let handle = spawn_loopback().await.expect("spawn loopback");
    assert!(handle.port >= 1024, "ephemeral port must be unprivileged");
    assert_eq!(
        handle.redirect_uri,
        format!("http://127.0.0.1:{}/oauth/callback", handle.port)
    );

    let redirect = handle.redirect_uri.clone();
    let driver = tokio::spawn(async move {
        drive_callback(&redirect, "code=auth_code_42&state=csrf_token_99").await;
    });

    let got = handle.await_callback(NETWORK_TIMEOUT).await.unwrap();
    driver.await.unwrap();

    assert_eq!(
        got,
        CallbackParams {
            code: "auth_code_42".into(),
            state: "csrf_token_99".into(),
        }
    );
}

#[tokio::test]
async fn end_to_end_provider_error_reaches_caller() {
    let handle = spawn_loopback().await.expect("spawn loopback");
    let redirect = handle.redirect_uri.clone();
    let driver = tokio::spawn(async move {
        drive_callback(
            &redirect,
            "error=access_denied&error_description=user%20said%20no",
        )
        .await;
    });

    let err = handle.await_callback(NETWORK_TIMEOUT).await.unwrap_err();
    driver.await.unwrap();

    assert_eq!(
        err,
        OAuthCallbackError::ProviderError {
            error: "access_denied".into(),
            description: Some("user said no".into()),
        }
    );
}

#[tokio::test]
async fn end_to_end_timeout_when_no_redirect_arrives() {
    let handle = spawn_loopback().await.expect("spawn loopback");
    let err = handle
        .await_callback(Duration::from_millis(150))
        .await
        .unwrap_err();
    assert_eq!(err, OAuthCallbackError::Timeout(Duration::from_millis(150)));
}
