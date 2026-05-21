//! Tests for [`super::github`]. Mirrors the Google test surface; only
//! GitHub-specific knobs (Accept header, 200-with-`error`-payload trap,
//! optional `expires_in`/`refresh_token`) need first-class assertions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use tokio::sync::Mutex;
use url::Url;

use super::github::{
    build_auth_url, AuthUrlParams, GithubClient, TokenResponse, AUTH_ENDPOINT, DEFAULT_SCOPES,
};
use super::TokenError;

// ── build_auth_url (pure) ───────────────────────────────────────────────

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
        client_id: "Iv1.0123456789abcdef",
        redirect_uri: "http://127.0.0.1:54321/oauth/callback",
        scopes: DEFAULT_SCOPES,
        state: "csrf_state_abc",
        code_challenge: "EXAMPLE_CHALLENGE",
    }
}

#[test]
fn auth_url_points_at_github_authorize_endpoint() {
    let (base, _) = parse(&build_auth_url(&sample_params()));
    assert_eq!(base, AUTH_ENDPOINT);
}

#[test]
fn auth_url_carries_all_required_keys() {
    let (_, params) = parse(&build_auth_url(&sample_params()));
    for key in [
        "client_id",
        "redirect_uri",
        "response_type",
        "scope",
        "state",
        "code_challenge",
        "code_challenge_method",
        "allow_signup",
    ] {
        assert!(
            params.contains_key(key),
            "auth URL missing required key '{key}': {params:?}"
        );
    }
}

#[test]
fn auth_url_pins_fixed_values() {
    let (_, params) = parse(&build_auth_url(&sample_params()));
    assert_eq!(
        params.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(params.get("allow_signup").map(String::as_str), Some("true"));
}

#[test]
fn auth_url_does_not_include_google_isms() {
    // Guard against a copy-paste regression where someone leaves
    // Google-only parameters (access_type, prompt, include_granted_scopes)
    // in the GitHub builder — GitHub silently ignores them, which would
    // mask a real bug elsewhere.
    let (_, params) = parse(&build_auth_url(&sample_params()));
    for forbidden in ["access_type", "prompt", "include_granted_scopes"] {
        assert!(
            !params.contains_key(forbidden),
            "GitHub auth URL must NOT carry the Google-only key '{forbidden}'"
        );
    }
}

#[test]
fn auth_url_joins_scopes_with_spaces() {
    let (_, params) = parse(&build_auth_url(&sample_params()));
    assert_eq!(params.get("scope").unwrap(), &DEFAULT_SCOPES.join(" "));
}

#[test]
fn default_scopes_cover_repo_and_user() {
    for needed in ["repo", "read:user"] {
        assert!(
            DEFAULT_SCOPES.contains(&needed),
            "DEFAULT_SCOPES is missing required scope '{needed}'"
        );
    }
}

// ── Token endpoint (GithubClient) ───────────────────────────────────────

#[derive(Default)]
struct MockTokenServer {
    requests: Arc<Mutex<Vec<(HeaderMap, Vec<(String, String)>)>>>,
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

    async fn last_request(&self) -> (HeaderMap, Vec<(String, String)>) {
        self.requests
            .lock()
            .await
            .last()
            .cloned()
            .expect("mock token endpoint received no requests")
    }
}

async fn handle_token(
    State(server): State<Arc<MockTokenServer>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let form: Vec<(String, String)> = url::form_urlencoded::parse(body.as_ref())
        .into_owned()
        .collect();
    server.requests.lock().await.push((headers, form));
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

#[tokio::test]
async fn exchange_code_sets_accept_application_json_header() {
    // Without Accept: application/json GitHub returns form-encoded.
    // This header is load-bearing — guard it explicitly.
    let body = r#"{
        "access_token": "gho_value",
        "scope": "repo,read:user",
        "token_type": "bearer"
    }"#;
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, body).await;

    let client = GithubClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let _ = client
        .exchange_code("http://127.0.0.1:55555/oauth/callback", "CODE", "VERIFIER")
        .await
        .expect("exchange should succeed");

    let (headers, _form) = mock.last_request().await;
    let accept = headers
        .get(reqwest::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        accept.contains("application/json"),
        "expected `Accept: application/json` on token request, got {accept:?}"
    );
}

#[tokio::test]
async fn exchange_code_posts_authorization_code_grant_form_encoded() {
    let body = r#"{"access_token":"gho","scope":"repo","token_type":"bearer"}"#;
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, body).await;

    let client = GithubClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let _ = client
        .exchange_code("http://127.0.0.1:55555/oauth/callback", "CODE", "VERIFIER")
        .await
        .unwrap();

    let (_, form) = mock.last_request().await;
    let kv: HashMap<_, _> = form.into_iter().collect();
    assert_eq!(
        kv.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert_eq!(kv.get("code").map(String::as_str), Some("CODE"));
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
async fn exchange_code_parses_response_without_expires_in_or_refresh_token() {
    // Classic OAuth-App response shape — no expiry, no refresh token.
    // TokenResponse must accept this without complaint.
    let body = r#"{
        "access_token": "gho_classic",
        "scope": "repo read:user",
        "token_type": "bearer"
    }"#;
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, body).await;

    let client = GithubClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let got = client
        .exchange_code("http://127.0.0.1:55555/oauth/callback", "C", "V")
        .await
        .unwrap();

    assert_eq!(
        got,
        TokenResponse {
            access_token: "gho_classic".into(),
            scope: "repo read:user".into(),
            token_type: "bearer".into(),
            expires_in: None,
            refresh_token: None,
            refresh_token_expires_in: None,
        }
    );
}

#[tokio::test]
async fn exchange_code_parses_full_expiring_token_response() {
    let body = r#"{
        "access_token": "gho_expiring",
        "scope": "repo",
        "token_type": "bearer",
        "expires_in": 28800,
        "refresh_token": "ghr_refresh",
        "refresh_token_expires_in": 15897600
    }"#;
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, body).await;

    let client = GithubClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let got = client
        .exchange_code("http://127.0.0.1:55555/oauth/callback", "C", "V")
        .await
        .unwrap();

    assert_eq!(got.expires_in, Some(28800));
    assert_eq!(got.refresh_token.as_deref(), Some("ghr_refresh"));
    assert_eq!(got.refresh_token_expires_in, Some(15897600));
}

#[tokio::test]
async fn exchange_code_traps_github_200_with_error_payload() {
    // GitHub's quirk: bad_verification_code is returned as HTTP 200
    // with a JSON `{"error":"bad_verification_code", …}` body instead
    // of a 4xx status. The client must detect this and surface a
    // typed Http error rather than silently storing an empty token.
    let body = r#"{
        "error": "bad_verification_code",
        "error_description": "The code passed is incorrect or expired."
    }"#;
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, body).await;

    let client = GithubClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let err = client
        .exchange_code("http://127.0.0.1:55555/oauth/callback", "BAD", "V")
        .await
        .unwrap_err();

    match err {
        TokenError::Http { status, body: got } => {
            assert_eq!(status, 200);
            assert!(
                got.contains("bad_verification_code"),
                "error body must include the raw github error_description: {got}"
            );
        }
        other => panic!("expected Http with raw body, got {other:?}"),
    }
}

#[tokio::test]
async fn refresh_access_token_posts_refresh_grant_with_only_three_fields() {
    let body = r#"{
        "access_token": "gho_refreshed",
        "scope": "repo",
        "token_type": "bearer",
        "expires_in": 28800
    }"#;
    let (endpoint, mock) = MockTokenServer::default().start().await;
    mock.set_response(StatusCode::OK, body).await;

    let client = GithubClient::new(http_client(), "cid").with_token_endpoint(endpoint);
    let _ = client.refresh_access_token("stored_refresh").await.unwrap();

    let (_, form) = mock.last_request().await;
    let kv: HashMap<_, _> = form.into_iter().collect();
    assert_eq!(
        kv.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        kv.get("refresh_token").map(String::as_str),
        Some("stored_refresh")
    );
    assert_eq!(kv.get("client_id").map(String::as_str), Some("cid"));
    assert!(!kv.contains_key("redirect_uri"));
    assert!(!kv.contains_key("code"));
    assert!(!kv.contains_key("code_verifier"));
}
