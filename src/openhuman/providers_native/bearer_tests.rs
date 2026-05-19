//! Tests for `super::bearer::AuthedClient` and `super::load_access_token`.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Mutex;

use crate::openhuman::credentials::profiles::TokenSet;
use crate::openhuman::credentials::AuthService;

use super::bearer::AuthedClient;
use super::load_access_token;

const PROVIDER: &str = "google";

fn fresh_service(token: Option<&str>) -> (TempDir, AuthService) {
    let dir = TempDir::new().unwrap();
    let svc = AuthService::new(dir.path(), true);
    if let Some(t) = token {
        let ts = TokenSet {
            access_token: t.into(),
            refresh_token: None,
            id_token: None,
            expires_at: None,
            token_type: Some("Bearer".into()),
            scope: Some("test".into()),
        };
        svc.store_provider_oauth_tokens(PROVIDER, "default", ts, Default::default(), true)
            .unwrap();
    }
    (dir, svc)
}

#[derive(Default, Clone)]
struct Recorded {
    last_authorization: Arc<Mutex<Option<String>>>,
    last_body: Arc<Mutex<Option<serde_json::Value>>>,
}

async fn record_auth(state: State<Recorded>, headers: HeaderMap) -> Json<serde_json::Value> {
    *state.last_authorization.lock().await = headers
        .get(reqwest::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    Json(json!({"ok": true}))
}

async fn record_post(
    state: State<Recorded>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    *state.last_authorization.lock().await = headers
        .get(reqwest::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    *state.last_body.lock().await = Some(body);
    Json(json!({"echoed": true}))
}

async fn always_401() -> (StatusCode, String) {
    (
        StatusCode::UNAUTHORIZED,
        String::from(r#"{"error":"expired"}"#),
    )
}

async fn always_500() -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, String::from("boom"))
}

async fn record_delete(state: State<Recorded>, headers: HeaderMap) -> StatusCode {
    *state.last_authorization.lock().await = headers
        .get(reqwest::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    StatusCode::NO_CONTENT
}

async fn start_server(state: Recorded) -> String {
    let app = Router::new()
        .route("/get", get(record_auth))
        .route("/post", post(record_post))
        .route("/delete", delete(record_delete))
        .route("/401", get(always_401))
        .route("/500", get(always_500))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{}", addr.port())
}

#[derive(Debug, Deserialize)]
struct Ok {
    ok: bool,
}

#[test]
fn load_access_token_errors_when_no_profile_exists() {
    let (_d, svc) = fresh_service(None);
    let err = load_access_token(&svc, PROVIDER).unwrap_err();
    assert!(
        err.to_string().contains("no connected account"),
        "expected 'no connected account' guidance, got: {err}"
    );
}

#[test]
fn load_access_token_returns_stored_token() {
    let (_d, svc) = fresh_service(Some("ya29.test"));
    let got = load_access_token(&svc, PROVIDER).unwrap();
    assert_eq!(got, "ya29.test");
}

#[tokio::test]
async fn get_json_attaches_bearer_header() {
    let (_d, svc) = fresh_service(Some("ya29.test"));
    let state = Recorded::default();
    let base = start_server(state.clone()).await;
    let http = reqwest::Client::new();
    let client = AuthedClient::new(&http, &svc, PROVIDER);
    let _: Ok = client.get_json(&format!("{base}/get")).await.unwrap();
    let auth = state.last_authorization.lock().await.clone().unwrap();
    assert_eq!(auth, "Bearer ya29.test");
}

#[tokio::test]
async fn post_json_sends_body_and_decodes_response() {
    let (_d, svc) = fresh_service(Some("ya29.test"));
    let state = Recorded::default();
    let base = start_server(state.clone()).await;
    let http = reqwest::Client::new();
    let client = AuthedClient::new(&http, &svc, PROVIDER);

    #[derive(Debug, Deserialize)]
    struct Echo {
        echoed: bool,
    }
    let body = json!({"hello": "world"});
    let resp: Echo = client
        .post_json(&format!("{base}/post"), &body)
        .await
        .unwrap();
    assert!(resp.echoed);
    let sent = state.last_body.lock().await.clone().unwrap();
    assert_eq!(sent, body);
}

#[tokio::test]
async fn delete_returns_unit_on_2xx() {
    let (_d, svc) = fresh_service(Some("ya29.test"));
    let state = Recorded::default();
    let base = start_server(state.clone()).await;
    let http = reqwest::Client::new();
    let client = AuthedClient::new(&http, &svc, PROVIDER);
    client.delete(&format!("{base}/delete")).await.unwrap();
    let auth = state.last_authorization.lock().await.clone().unwrap();
    assert_eq!(auth, "Bearer ya29.test");
}

#[tokio::test]
async fn http_401_surfaces_token_expired_guidance() {
    let (_d, svc) = fresh_service(Some("ya29.test"));
    let state = Recorded::default();
    let base = start_server(state).await;
    let http = reqwest::Client::new();
    let client = AuthedClient::new(&http, &svc, PROVIDER);
    let err: anyhow::Error = client
        .get_json::<serde_json::Value>(&format!("{base}/401"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("HTTP 401") || err.to_string().contains("unauthorized"),
        "401 must surface as a typed unauthorized error: {err}"
    );
}

#[tokio::test]
async fn http_500_surfaces_raw_body() {
    let (_d, svc) = fresh_service(Some("ya29.test"));
    let state = Recorded::default();
    let base = start_server(state).await;
    let http = reqwest::Client::new();
    let client = AuthedClient::new(&http, &svc, PROVIDER);
    let err = client
        .get_json::<serde_json::Value>(&format!("{base}/500"))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("500"), "must mention HTTP 500: {msg}");
    assert!(msg.contains("boom"), "must include raw body 'boom': {msg}");
}
