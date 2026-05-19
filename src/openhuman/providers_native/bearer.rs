//! Small wrapper around `reqwest::RequestBuilder` that attaches the
//! provider's stored Bearer token, fires the request, and surfaces a
//! typed error on non-2xx. Pulled into a module so each provider's
//! per-operation function reads as "build URL + body, send" with no
//! auth boilerplate.

use anyhow::{anyhow, Result};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::openhuman::credentials::AuthService;

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
    /// any 2xx and a typed error otherwise.
    pub async fn delete(&self, url: &str) -> Result<()> {
        let token = load_access_token(self.service, self.provider)?;
        let resp = self
            .http
            .request(Method::DELETE, url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| anyhow!("DELETE {url} failed: {e}"))?;
        ensure_2xx(resp).await.map(|_| ())
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<T> {
        let token = load_access_token(self.service, self.provider)?;
        let mut req: RequestBuilder = self.http.request(method.clone(), url).bearer_auth(&token);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("{method} {url} failed: {e}"))?;
        let body = ensure_2xx(resp).await?;
        serde_json::from_str::<T>(&body)
            .map_err(|e| anyhow!("decode {method} {url} response: {e}; body={body}"))
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
