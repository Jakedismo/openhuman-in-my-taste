//! Tests for the [`super::ops`] orchestrator. Covers:
//!   * client-id-missing surfacing the right typed error
//!   * auth-url shape (carries the live loopback redirect, valid params)
//!   * end-to-end happy path: build flow → drive loopback → mock token
//!     endpoint → tokens persisted via AuthService
//!   * state mismatch detection (CSRF guard)
//!   * loopback timeout surfacing

use std::sync::Arc;
use std::time::Duration;

use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Router};
use tempfile::TempDir;
use tokio::sync::Mutex;
use url::Url;

use crate::openhuman::credentials::AuthService;

use super::ops::{
    start_github_flow, start_github_flow_with, start_google_flow, start_google_flow_with,
    OAuthFlowError, GITHUB_CLIENT_ID, GOOGLE_CLIENT_ID,
};
use super::persistence::{GITHUB_PROVIDER, GOOGLE_PROVIDER};

const NETWORK_TIMEOUT: Duration = Duration::from_secs(3);

// ── shared mock token server ────────────────────────────────────────────

#[derive(Default)]
struct MockToken {
    next_response: Arc<Mutex<Option<(StatusCode, String)>>>,
}

impl MockToken {
    async fn start(self) -> (String, Arc<Self>) {
        let me = Arc::new(self);
        let app = Router::new()
            .route("/token", post(handle))
            .with_state(me.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{}/token", addr.port()), me)
    }

    async fn set(&self, status: StatusCode, body: impl Into<String>) {
        *self.next_response.lock().await = Some((status, body.into()));
    }
}

async fn handle(State(srv): State<Arc<MockToken>>, _body: Bytes) -> (StatusCode, String) {
    srv.next_response.lock().await.clone().unwrap_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        String::from("no response set"),
    ))
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(NETWORK_TIMEOUT)
        .build()
        .unwrap()
}

async fn drive_loopback_redirect(redirect_uri: &str, code: &str, state: &str) {
    let url = format!("{redirect_uri}?code={code}&state={state}");
    let _ = reqwest::Client::new().get(&url).send().await.unwrap();
}

fn fresh_service() -> (TempDir, AuthService) {
    let dir = TempDir::new().unwrap();
    let svc = AuthService::new(dir.path(), true);
    (dir, svc)
}

// ── client-id resolution ────────────────────────────────────────────────

#[tokio::test]
async fn start_google_flow_without_baked_client_id_reports_typed_error() {
    if GOOGLE_CLIENT_ID.is_some() {
        // Build configured the env var; skip the missing-config test. It
        // still passes via `start_google_flow_with` paths below.
        return;
    }
    match start_google_flow(http_client()).await {
        Ok(_) => panic!("expected ClientIdMissing"),
        Err(OAuthFlowError::ClientIdMissing { provider, env_var }) => {
            assert_eq!(provider, "google");
            assert_eq!(env_var, "OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID");
        }
        Err(other) => panic!("expected ClientIdMissing, got {other:?}"),
    }
}

#[tokio::test]
async fn start_github_flow_without_baked_client_id_reports_typed_error() {
    if GITHUB_CLIENT_ID.is_some() {
        return;
    }
    match start_github_flow(http_client()).await {
        Ok(_) => panic!("expected ClientIdMissing"),
        Err(OAuthFlowError::ClientIdMissing { provider, env_var }) => {
            assert_eq!(provider, "github");
            assert_eq!(env_var, "OPENHUMAN_GITHUB_OAUTH_CLIENT_ID");
        }
        Err(other) => panic!("expected ClientIdMissing, got {other:?}"),
    }
}

// ── auth-url shape ──────────────────────────────────────────────────────

#[tokio::test]
async fn google_flow_emits_auth_url_with_live_loopback_redirect() {
    let flow = start_google_flow_with(http_client(), "test-google-cid", None)
        .await
        .unwrap();
    let parsed = Url::parse(&flow.auth_url).unwrap();
    assert_eq!(parsed.host_str(), Some("accounts.google.com"));

    let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some("test-google-cid")
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some(flow.redirect_uri.as_str())
    );
    // The state and code_challenge must be non-empty — these are what
    // bind the loopback to this specific flow.
    assert!(!params.get("state").unwrap().is_empty());
    assert!(!params.get("code_challenge").unwrap().is_empty());
}

#[tokio::test]
async fn github_flow_emits_auth_url_with_live_loopback_redirect() {
    let flow = start_github_flow_with(http_client(), "Iv1.test")
        .await
        .unwrap();
    let parsed = Url::parse(&flow.auth_url).unwrap();
    assert_eq!(parsed.host_str(), Some("github.com"));
    let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some("Iv1.test")
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some(flow.redirect_uri.as_str())
    );
}

// ── happy path ──────────────────────────────────────────────────────────

#[tokio::test]
async fn google_flow_happy_path_persists_tokens() {
    let (token_endpoint, mock) = MockToken::default().start().await;
    mock.set(
        StatusCode::OK,
        r#"{
            "access_token": "ya29.mock",
            "refresh_token": "1//rt_mock",
            "expires_in": 3599,
            "scope": "openid email",
            "token_type": "Bearer"
        }"#,
    )
    .await;

    let flow = start_google_flow_with(http_client(), "cid", None)
        .await
        .unwrap()
        .with_token_endpoint(token_endpoint);

    // Grab the values we need to synthesize a redirect (state) before
    // moving `flow` into `complete()`.
    let redirect_uri = flow.redirect_uri.clone();
    let state = url_state_param(&flow.auth_url);
    let driver = tokio::spawn(async move {
        drive_loopback_redirect(&redirect_uri, "AUTH_CODE", &state).await;
    });

    let (_dir, service) = fresh_service();
    let completion = flow
        .complete(&service, "default", NETWORK_TIMEOUT)
        .await
        .unwrap();
    driver.await.unwrap();

    assert_eq!(completion.provider, GOOGLE_PROVIDER);
    let stored = service.get_profile(GOOGLE_PROVIDER, None).unwrap().unwrap();
    let ts = stored.token_set.unwrap();
    assert_eq!(ts.access_token, "ya29.mock");
    assert_eq!(ts.refresh_token.as_deref(), Some("1//rt_mock"));
}

#[tokio::test]
async fn github_flow_happy_path_persists_tokens() {
    let (token_endpoint, mock) = MockToken::default().start().await;
    mock.set(
        StatusCode::OK,
        r#"{
            "access_token": "gho_mock",
            "scope": "repo",
            "token_type": "bearer"
        }"#,
    )
    .await;

    let flow = start_github_flow_with(http_client(), "Iv1.mock")
        .await
        .unwrap()
        .with_token_endpoint(token_endpoint);

    let redirect_uri = flow.redirect_uri.clone();
    let state = url_state_param(&flow.auth_url);
    let driver = tokio::spawn(async move {
        drive_loopback_redirect(&redirect_uri, "GH_CODE", &state).await;
    });

    let (_dir, service) = fresh_service();
    let completion = flow
        .complete(&service, "default", NETWORK_TIMEOUT)
        .await
        .unwrap();
    driver.await.unwrap();

    assert_eq!(completion.provider, GITHUB_PROVIDER);
    let stored = service.get_profile(GITHUB_PROVIDER, None).unwrap().unwrap();
    assert_eq!(stored.token_set.unwrap().access_token, "gho_mock");
}

// ── CSRF + timeout guards ───────────────────────────────────────────────

#[tokio::test]
async fn state_mismatch_refuses_to_exchange_code() {
    // The mock token endpoint MUST NOT be hit if state mismatches —
    // assert by leaving it on the default 500 error.
    let (token_endpoint, _mock) = MockToken::default().start().await;
    let flow = start_google_flow_with(http_client(), "cid", None)
        .await
        .unwrap()
        .with_token_endpoint(token_endpoint);

    let redirect_uri = flow.redirect_uri.clone();
    let driver = tokio::spawn(async move {
        drive_loopback_redirect(&redirect_uri, "CODE", "WRONG_STATE").await;
    });

    let (_dir, service) = fresh_service();
    let err = flow
        .complete(&service, "default", NETWORK_TIMEOUT)
        .await
        .unwrap_err();
    driver.await.unwrap();

    assert!(
        matches!(err, OAuthFlowError::StateMismatch),
        "expected StateMismatch, got {err:?}"
    );
    // And nothing was persisted.
    assert!(service
        .get_profile(GOOGLE_PROVIDER, None)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn flow_completes_with_loopback_timeout_when_no_redirect_arrives() {
    let (token_endpoint, _mock) = MockToken::default().start().await;
    let flow = start_google_flow_with(http_client(), "cid", None)
        .await
        .unwrap()
        .with_token_endpoint(token_endpoint);

    let (_dir, service) = fresh_service();
    let err = flow
        .complete(&service, "default", Duration::from_millis(150))
        .await
        .unwrap_err();

    // The Callback variant wraps an OAuthCallbackError::Timeout; just
    // assert the variant rather than the inner duration to avoid
    // timing flakes.
    assert!(
        matches!(err, OAuthFlowError::Callback(_)),
        "expected Callback variant on timeout, got {err:?}"
    );
}

// ── helpers ─────────────────────────────────────────────────────────────

fn url_state_param(auth_url: &str) -> String {
    Url::parse(auth_url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .expect("auth_url must carry a `state` param")
}
