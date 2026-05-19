//! GitHub OAuth 2.0 + PKCE provider.
//!
//! Targets the public OAuth-App endpoints documented at
//! <https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps>.
//! OAuth Apps gained PKCE support in early 2024; we use S256 from the
//! same `crate::openhuman::oauth::pkce` primitives that drive Google.
//!
//! Differences from the Google client worth flagging:
//!   * GitHub's token endpoint defaults to a `x-www-form-urlencoded`
//!     **response** body unless the request carries
//!     `Accept: application/json`. Omitting that header silently breaks
//!     JSON decoding, so the client sets it explicitly.
//!   * `expires_in` is OPTIONAL on the GitHub side — long-lived OAuth
//!     App user tokens never expire, only opt-in expiring tokens do.
//!   * `refresh_token` is OPTIONAL — only OAuth Apps that have enabled
//!     "Expire user authorization tokens" return one.

use serde::Deserialize;

use super::TokenError;

/// Authorization endpoint — user-agent redirected here to consent.
pub const AUTH_ENDPOINT: &str = "https://github.com/login/oauth/authorize";

/// Token endpoint — server-to-server POST for code exchange and refresh.
pub const TOKEN_ENDPOINT: &str = "https://github.com/login/oauth/access_token";

/// Default scopes requested for a fresh GitHub connection. Covers the
/// public-API + per-user repo read/write surface Composio previously
/// brokered. Narrower than Google's because GitHub bundles many
/// capabilities under broader scopes (e.g. `repo` covers issues, PRs,
/// contents, …).
pub const DEFAULT_SCOPES: &[&str] = &["repo", "read:user", "user:email"];

/// Parameters for [`build_auth_url`]. Mirrors the Google variant — kept
/// per-provider deliberately so future provider-specific knobs (e.g.
/// GitHub's `allow_signup`) can land without breaking the cross-cutting
/// type.
pub struct AuthUrlParams<'a> {
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scopes: &'a [&'a str],
    pub state: &'a str,
    pub code_challenge: &'a str,
}

/// Build the GitHub authorization URL with PKCE S256.
///
/// Pinned parameter set:
///   * `response_type=code`            — authorization-code flow
///   * `code_challenge_method=S256`    — PKCE
///   * `allow_signup=true`             — match GitHub's default; spelled
///     out so a future hardening pass can flip it without re-reading
///     this code path.
///
/// GitHub does not have a `prompt` / `access_type` / `include_granted_scopes`
/// equivalent — those are Google-isms.
pub fn build_auth_url(params: &AuthUrlParams<'_>) -> String {
    let scope = params.scopes.join(" ");
    let pairs: [(&str, &str); 8] = [
        ("client_id", params.client_id),
        ("redirect_uri", params.redirect_uri),
        ("response_type", "code"),
        ("scope", &scope),
        ("state", params.state),
        ("code_challenge", params.code_challenge),
        ("code_challenge_method", "S256"),
        ("allow_signup", "true"),
    ];
    url::Url::parse_with_params(AUTH_ENDPOINT, &pairs)
        .expect("AUTH_ENDPOINT is a known-good URL")
        .into()
}

/// Successful response from GitHub's token endpoint. Shape pinned to
/// the OAuth Apps JSON variant (requires `Accept: application/json` —
/// the form-encoded fallback is intentionally not supported here).
///
/// `expires_in` and `refresh_token` are both `Option` because GitHub
/// only populates them on OAuth Apps with token-expiry enabled.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TokenResponse {
    pub access_token: String,
    pub scope: String,
    pub token_type: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub refresh_token_expires_in: Option<u64>,
}

/// Thin HTTPS client for GitHub's token endpoint. Holds the `client_id`
/// and a reusable `reqwest::Client`; the token endpoint is fixed to
/// [`TOKEN_ENDPOINT`] in production but overridable in tests via
/// [`GithubClient::with_token_endpoint`].
#[derive(Clone)]
pub struct GithubClient {
    http: reqwest::Client,
    client_id: String,
    token_endpoint: String,
}

impl GithubClient {
    pub fn new(http: reqwest::Client, client_id: impl Into<String>) -> Self {
        Self {
            http,
            client_id: client_id.into(),
            token_endpoint: TOKEN_ENDPOINT.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_token_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.token_endpoint = endpoint.into();
        self
    }

    /// Exchange the authorization `code` returned by the loopback
    /// redirect for an access token. `code_verifier` is the matching
    /// PKCE verifier from [`crate::openhuman::oauth::pkce`].
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

    /// Trade a stored refresh token for a fresh access token. Only
    /// applicable to OAuth Apps with the "Expire user authorization
    /// tokens" feature enabled — for classic OAuth Apps, calling this
    /// returns an HTTP error.
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
            // Without `Accept: application/json` GitHub returns a
            // form-encoded body that serde_json cannot parse.
            .header(reqwest::header::ACCEPT, "application/json")
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
        // GitHub returns 200 + JSON `{"error":"bad_verification_code", …}`
        // on auth failures instead of an HTTP error status. Treat the
        // presence of an `error` key as a failure so callers do not
        // silently store an empty access_token.
        if body.contains("\"error\"") && !body.contains("\"access_token\"") {
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
