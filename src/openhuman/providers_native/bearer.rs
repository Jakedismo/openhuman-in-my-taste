//! Small wrapper around `reqwest::RequestBuilder` that attaches the
//! provider's stored Bearer token, fires the request, and surfaces a
//! typed error on non-2xx. Pulled into a module so each provider's
//! per-operation function reads as "build URL + body, send" with no
//! auth boilerplate.

use anyhow::{anyhow, Result};
use reqwest::{Method, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::openhuman::credentials::AuthService;
use crate::openhuman::oauth::refresh::refresh_provider_token;

use super::load_access_token;

/// Authenticated request builder. Wraps a `reqwest::Client` + the
/// access token for a single provider. One instance per call site is
/// fine — both fields are cheap clones / shared references.
pub struct AuthedClient<'a> {
    http: &'a reqwest::Client,
    service: &'a AuthService,
    provider: &'static str,
}

impl<'a> AuthedClient<'a> {
    pub fn new(
        http: &'a reqwest::Client,
        service: &'a AuthService,
        provider: &'static str,
    ) -> Self {
        Self {
            http,
            service,
            provider,
        }
    }

    /// `GET <url>` with the Bearer token attached. Decodes the response
    /// as JSON.
    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        self.send_json(Method::GET, url, None).await
    }

    /// `POST <url>` with JSON `body` and the Bearer token attached.
    /// Decodes the response as JSON.
    pub async fn post_json<T: DeserializeOwned>(&self, url: &str, body: &Value) -> Result<T> {
        self.send_json(Method::POST, url, Some(body)).await
    }

    /// `DELETE <url>` with the Bearer token attached. Returns `()` on
    /// any 2xx and a typed error otherwise. Transparently refreshes
    /// the access token + retries once on HTTP 401.
    pub async fn delete(&self, url: &str) -> Result<()> {
        let resp = self.fire_once(Method::DELETE, url, None).await?;
        let resp = self
            .maybe_refresh_and_retry(resp, Method::DELETE, url, None)
            .await?;
        ensure_2xx(resp).await.map(|_| ())
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<T> {
        let resp = self.fire_once(method.clone(), url, body).await?;
        let resp = self
            .maybe_refresh_and_retry(resp, method.clone(), url, body)
            .await?;
        let body_text = ensure_2xx(resp).await?;
        serde_json::from_str::<T>(&body_text)
            .map_err(|e| anyhow!("decode {method} {url} response: {e}; body={body_text}"))
    }

    /// Build + send a single request with the current bearer attached.
    /// Returns the raw `Response` so the caller can inspect status
    /// before deciding whether to refresh-and-retry.
    async fn fire_once(&self, method: Method, url: &str, body: Option<&Value>) -> Result<Response> {
        let token = load_access_token(self.service, self.provider)?;
        let mut req = self.http.request(method.clone(), url).bearer_auth(&token);
        if let Some(b) = body {
            req = req.json(b);
        }
        req.send()
            .await
            .map_err(|e| anyhow!("{method} {url} failed: {e}"))
    }

    /// If `resp` is HTTP 401 and the stored profile has a refresh
    /// token, refresh + retry exactly once with the fresh access
    /// token. Any refresh failure (missing client_id, no refresh
    /// token, provider rejection) surfaces the ORIGINAL 401 so the
    /// caller sees an authentic provider error — never the refresh
    /// failure, which would obscure the underlying cause.
    async fn maybe_refresh_and_retry(
        &self,
        resp: Response,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Response> {
        if resp.status() != StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        match refresh_provider_token(self.http, self.service, self.provider).await {
            Ok(_) => {
                tracing::debug!(
                    provider = %self.provider,
                    "[bearer] 401 → refresh ok, retrying once"
                );
                self.fire_once(method, url, body).await
            }
            Err(e) => {
                tracing::debug!(
                    provider = %self.provider,
                    error = %e,
                    "[bearer] 401 → refresh failed, surfacing original 401"
                );
                Ok(resp)
            }
        }
    }
}

/// If `resp` is 2xx, return the body as a string. Otherwise return a
/// typed error containing the status + body verbatim.
async fn ensure_2xx(resp: Response) -> Result<String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| anyhow!("reading response body: {e}"))?;
    if status == StatusCode::UNAUTHORIZED {
        // 401 is the common refresh-trigger. Surface it as its own
        // shape so callers (or a future retry-on-refresh wrapper) can
        // distinguish it from generic 4xx.
        return Err(anyhow!(
            "unauthorized (HTTP 401) — token likely expired or revoked: {body}"
        ));
    }
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {body}"));
    }
    Ok(body)
}
