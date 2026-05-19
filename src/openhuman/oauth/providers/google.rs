//! Google OAuth 2.0 + PKCE provider.
//!
//! Wire formats are pinned to the public endpoints documented at
//! <https://developers.google.com/identity/protocols/oauth2/native-app>.
//! In the native-app flow we use the installed-application client type
//! with a `127.0.0.1` loopback redirect (RFC 8252 §7.3) and PKCE S256.

use serde::Deserialize;
use thiserror::Error;

/// Authorization endpoint — user-agent redirected here to consent.
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Token endpoint — server-to-server POST for code exchange and refresh.
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// Default scopes requested for a fresh Google connection. Covers the
/// Gmail + Calendar + Drive surface area that Composio previously
/// brokered, plus `openid`/`email`/`profile` so we can identify the
/// connected account for the UI.
///
/// `drive.file` is the per-file scope (not full Drive) — strictly what
/// the user picks. `gmail.readonly` + `gmail.send` are the read/send
/// pair without the destructive `gmail.modify`/`gmail.compose` scopes.
pub const DEFAULT_SCOPES: &[&str] = &[
    "openid",
    "email",
    "profile",
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.send",
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/calendar.events",
    "https://www.googleapis.com/auth/drive.file",
];

/// Parameters for [`build_auth_url`]. Borrowed slices so callers do not
/// have to allocate when they already have the values to hand.
pub struct AuthUrlParams<'a> {
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scopes: &'a [&'a str],
    pub state: &'a str,
    pub code_challenge: &'a str,
}

/// Build the Google authorization URL with PKCE S256 + offline access.
///
/// Pinned parameter set:
///   * `response_type=code`            — authorization-code flow
///   * `code_challenge_method=S256`    — PKCE
///   * `access_type=offline`           — ask Google for a refresh token
///   * `prompt=consent`                — force the consent screen so the
///     refresh token is always returned (Google omits it on subsequent
///     consents otherwise, and we have no other way to recover it).
///   * `include_granted_scopes=true`   — incremental auth — let later
///     flows widen scope without losing what the user already granted.
pub fn build_auth_url(params: &AuthUrlParams<'_>) -> String {
    let scope = params.scopes.join(" ");
    let pairs: [(&str, &str); 10] = [
        ("client_id", params.client_id),
        ("redirect_uri", params.redirect_uri),
        ("response_type", "code"),
        ("scope", &scope),
        ("state", params.state),
        ("code_challenge", params.code_challenge),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("include_granted_scopes", "true"),
    ];
    // `url::Url::parse_with_params` percent-encodes each value using the
    // application/x-www-form-urlencoded set, which is exactly what Google
    // expects on the query string.
    url::Url::parse_with_params(AUTH_ENDPOINT, &pairs)
        .expect("AUTH_ENDPOINT is a known-good URL")
        .into()
}

/// Successful response from Google's token endpoint. Fields are pinned
/// to the documented shape; unknown fields are tolerated (Google adds
/// new metadata occasionally).
///
/// `refresh_token` is `Option` because Google only returns one on the
/// first consent (and on subsequent consents when `prompt=consent` is
/// set). A refresh-grant response will not include a fresh refresh
/// token — callers must persist the original.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_in: u64,
    pub scope: String,
    pub token_type: String,
    #[serde(default)]
    pub id_token: Option<String>,
}

/// Errors surfaced by [`GoogleClient::exchange_code`] and
/// [`GoogleClient::refresh_access_token`].
#[derive(Debug, Error)]
pub enum TokenError {
    /// The provider returned a non-2xx HTTP status. `body` is the raw
    /// response body verbatim so callers can surface the
    /// `error_description` Google embeds in the JSON.
    #[error("google token endpoint returned HTTP {status}: {body}")]
    Http { status: u16, body: String },

    /// Underlying transport failed (DNS, TLS, connection reset, …).
    #[error("network error talking to google token endpoint: {0}")]
    Network(String),

    /// Provider returned 2xx but the JSON did not parse into
    /// [`TokenResponse`]. Carries the raw body for debugging.
    #[error("could not decode google token response: {message} (body={body})")]
    Decode { message: String, body: String },
}

/// Thin HTTPS client for the Google token endpoint. Holds the
/// `client_id` and a reusable `reqwest::Client`; the token endpoint
/// is fixed to [`TOKEN_ENDPOINT`] in production but overridable in
/// tests via [`GoogleClient::with_token_endpoint`].
#[derive(Clone)]
pub struct GoogleClient {
    http: reqwest::Client,
    client_id: String,
    token_endpoint: String,
}

impl GoogleClient {
    pub fn new(http: reqwest::Client, client_id: impl Into<String>) -> Self {
        Self {
            http,
            client_id: client_id.into(),
            token_endpoint: TOKEN_ENDPOINT.into(),
        }
    }

    /// Test-only knob to point the client at a mock token endpoint
    /// (typically a local axum server bound on `127.0.0.1:0`).
    #[cfg(test)]
    pub(crate) fn with_token_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.token_endpoint = endpoint.into();
        self
    }

    /// Exchange the authorization `code` returned by the loopback
    /// redirect for an access + refresh token pair. `code_verifier` is
    /// the matching PKCE verifier from [`crate::openhuman::oauth::pkce`].
    pub async fn exchange_code(
        &self,
        redirect_uri: &str,
        code: &str,
        code_verifier: &str,
    ) -> Result<TokenResponse, TokenError> {
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", self.client_id.as_str()),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ];
        self.post_token(&form).await
    }

    /// Trade a stored refresh token for a fresh access token. The
    /// response will NOT include a new `refresh_token` — Google reuses
    /// the existing one until the user revokes it.
    pub async fn refresh_access_token(
        &self,
        refresh_token: &str,
    ) -> Result<TokenResponse, TokenError> {
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.client_id.as_str()),
        ];
        self.post_token(&form).await
    }

    async fn post_token<'a>(
        &self,
        form: &[(&'a str, &'a str)],
    ) -> Result<TokenResponse, TokenError> {
        let resp = self
            .http
            .post(&self.token_endpoint)
            .form(form)
            .send()
            .await
            .map_err(|e| TokenError::Network(e.to_string()))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| TokenError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(TokenError::Http {
                status: status.as_u16(),
                body,
            });
        }
        serde_json::from_str::<TokenResponse>(&body).map_err(|e| TokenError::Decode {
            message: e.to_string(),
            body,
        })
    }
}
