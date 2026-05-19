//! Tests for [`super::google`]. Focus is on parameter pinning — every
//! key in [`build_auth_url`] is required to land exactly once, with the
//! exact value documented in the rustdoc. A silent regression on any
//! of them (e.g. losing `prompt=consent`) would silently break the
//! refresh-token recovery story without breaking any compile-time check.

use std::collections::HashMap;

use url::Url;

use std::sync::Arc;
use std::time::Duration;

use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Router};
use tokio::sync::Mutex;

use super::google::{
    build_auth_url, AuthUrlParams, GoogleClient, TokenError, TokenResponse, AUTH_ENDPOINT,
    DEFAULT_SCOPES,
};

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

// ── Token endpoint (GoogleClient) ───────────────────────────────────────

/// Mock-server harness: spawns an axum app on `127.0.0.1:0`, captures
/// every form-encoded request landing on `/token`, and lets the test
/// programmatically choose what status + body to return next.
#[derive(Default)]
struct MockTokenServer {
    requests: Arc<Mutex<Vec<Vec<(String, String)>>>>,
    next_response: Arc<Mutex<Option<(StatusCode, String)>>>,
}

impl MockTokenServer {
    async fn start(self) -> (String, Arc<Self>) {
        let me = Arc::new(self);
        let app_state = me.clone();
        let app = Router::new()
            .route("/token", post(handle_token))
            .with_state(app_state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{}/token", addr.port()), me)
    }

    async fn set_response(&self, status: StatusCode, body: impl Into<String>) {
        *self.next_response.lock().await = Some((status, body.into()));
    }

    async fn last_request(&self) -> Vec<(String, String)> {
        self.requests
            .lock()
            .await
            .last()
            .cloned()
            .expect("mock token endpoint received no requests")
    }

    async fn request_count(&self) -> usize {
        self.requests.lock().await.len()
    }
}

async fn handle_token(
    State(server): State<Arc<MockTokenServer>>,
    body: Bytes,
) -> (StatusCode, String) {
    // We do not enable `axum/form`, so parse the form-encoded body
    // ourselves. Same wire format Google would see in production.
    let form: Vec<(String, String)> = url::form_urlencoded::parse(body.as_ref())
        .into_owned()
        .collect();
    server.requests.lock().await.push(form);
    server.next_response.lock().await.clone().unwrap_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        String::from("no response set"),
    ))
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("reqwest client")
}

fn full_token_json() -> &'static str {
    r#"{
        "access_token": "ya29.access_value",
        "refresh_token": "1//refresh_value",
        "expires_in": 3599,
        "scope": "openid email https://www.googleapis.com/auth/gmail.readonly",
        "token_type": "Bearer",
        "id_token": "eyJraWQ.eyJzdWI.signed"
    }"#
}

#[tokio::test]
async fn exchange_code_posts_authorization_code_grant_form_encoded() {
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, full_token_json()).await;

    let client = GoogleClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let _ = client
        .exchange_code(
            "http://127.0.0.1:55555/oauth/callback",
            "AUTH_CODE",
            "VERIFIER",
        )
        .await
        .expect("exchange should succeed against the mock");

    let last = mock.last_request().await;
    let kv: std::collections::HashMap<_, _> = last.into_iter().collect();
    assert_eq!(
        kv.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert_eq!(kv.get("code").map(String::as_str), Some("AUTH_CODE"));
    assert_eq!(kv.get("client_id").map(String::as_str), Some("cid"));
    assert_eq!(
        kv.get("redirect_uri").map(String::as_str),
        Some("http://127.0.0.1:55555/oauth/callback")
    );
    assert_eq!(
        kv.get("code_verifier").map(String::as_str),
        Some("VERIFIER")
    );
}

#[tokio::test]
async fn exchange_code_parses_response_with_refresh_and_id_token() {
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, full_token_json()).await;

    let client = GoogleClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let got = client
        .exchange_code(
            "http://127.0.0.1:55555/oauth/callback",
            "AUTH_CODE",
            "VERIFIER",
        )
        .await
        .unwrap();

    assert_eq!(
        got,
        TokenResponse {
            access_token: "ya29.access_value".into(),
            refresh_token: Some("1//refresh_value".into()),
            expires_in: 3599,
            scope: "openid email https://www.googleapis.com/auth/gmail.readonly".into(),
            token_type: "Bearer".into(),
            id_token: Some("eyJraWQ.eyJzdWI.signed".into()),
        }
    );
}

#[tokio::test]
async fn exchange_code_surfaces_http_400_with_raw_body() {
    let (endpoint, mock) = MockTokenServer::default().start().await;
    let body = r#"{"error":"invalid_grant","error_description":"Bad code"}"#;
    mock.set_response(StatusCode::BAD_REQUEST, body).await;

    let client = GoogleClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let err = client
        .exchange_code("http://127.0.0.1:55555/oauth/callback", "BAD", "V")
        .await
        .unwrap_err();

    match err {
        TokenError::Http { status, body: got } => {
            assert_eq!(status, 400);
            assert_eq!(got, body);
        }
        other => panic!("expected Http(400), got {other:?}"),
    }
}

#[tokio::test]
async fn exchange_code_decode_error_includes_raw_body() {
    // 200 OK but non-JSON body — we should surface a Decode error with
    // the raw body so callers can debug. Network/Http branches must NOT
    // catch this case (the status was success).
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, "<html>not json</html>")
        .await;

    let client = GoogleClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let err = client
        .exchange_code("http://127.0.0.1:55555/oauth/callback", "C", "V")
        .await
        .unwrap_err();

    match err {
        TokenError::Decode { body, .. } => {
            assert_eq!(body, "<html>not json</html>");
        }
        other => panic!("expected Decode error, got {other:?}"),
    }
}

#[tokio::test]
async fn refresh_access_token_posts_refresh_grant_with_only_three_fields() {
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, full_token_json()).await;

    let client = GoogleClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let _ = client.refresh_access_token("stored_refresh").await.unwrap();

    let last = mock.last_request().await;
    let kv: std::collections::HashMap<_, _> = last.into_iter().collect();
    assert_eq!(
        kv.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        kv.get("refresh_token").map(String::as_str),
        Some("stored_refresh")
    );
    assert_eq!(kv.get("client_id").map(String::as_str), Some("cid"));
    // Refresh grant must NOT include redirect_uri / code / code_verifier
    // — Google rejects the request as invalid_grant otherwise.
    assert!(!kv.contains_key("redirect_uri"));
    assert!(!kv.contains_key("code"));
    assert!(!kv.contains_key("code_verifier"));
}

#[tokio::test]
async fn refresh_access_token_tolerates_response_without_new_refresh_token() {
    // Real Google behavior: on refresh, the response carries
    // access_token + expires_in + scope + token_type, but NOT a fresh
    // refresh_token. Decode must succeed and `refresh_token` must be
    // `None` so callers know to keep their stored copy.
    let body = r#"{
        "access_token": "new_access",
        "expires_in": 3599,
        "scope": "openid email",
        "token_type": "Bearer"
    }"#;
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, body).await;

    let client = GoogleClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let got = client.refresh_access_token("stored_refresh").await.unwrap();

    assert_eq!(got.access_token, "new_access");
    assert_eq!(got.refresh_token, None);
    assert_eq!(got.id_token, None);
}

#[tokio::test]
async fn exchange_code_increments_request_counter() {
    // Sanity check that the mock harness itself observes the request —
    // if this fails the other assertions could pass via the
    // "no requests received" path.
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, full_token_json()).await;

    let client = GoogleClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let _ = client
        .exchange_code("http://127.0.0.1:55555/oauth/callback", "C", "V")
        .await
        .unwrap();

    assert_eq!(mock.request_count().await, 1);
}
