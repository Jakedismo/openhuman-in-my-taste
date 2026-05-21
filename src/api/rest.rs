//! HTTP client for TinyHumans / AlphaHuman API routes (`/auth/...`, etc.).

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use reqwest::{Client, Method, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use super::jwt::bearer_authorization_value;

/// Typed errors surfaced by `authed_json` for expected backend states that
/// callers should recover from in-flow rather than funnel into Sentry.
#[derive(Debug, thiserror::Error)]
pub enum BackendApiError {
    /// Edit / delete of a channel message returned 404. Happens when the
    /// user deletes the message on the provider side (Telegram, Discord,
    /// Slack, …) but our local `StreamingState` still has the id, or when
    /// the backend GC'd the relay row before we got around to editing it.
    /// Callers should clear stale state and skip the retry. Targets
    /// `OPENHUMAN-TAURI-2Y` (~454 events on `/channels/telegram/messages/<id>`).
    #[error("message not found on {provider}: {message_id}")]
    MessageNotFound {
        /// Channel provider segment (e.g. `"telegram"`, `"discord"`).
        provider: String,
        /// Provider-specific message id from the URL.
        message_id: String,
    },
}

/// Extract `(provider, message_id)` from a backend channel path of the
/// shape `/channels/<provider>/messages/<id>`. Returns `None` for paths
/// with a different segment count or non-`channels` first segment.
fn parse_message_path(path: &str) -> Option<(&str, &str)> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() == 4 && segments[0] == "channels" && segments[2] == "messages" {
        return Some((segments[1], segments[3]));
    }
    None
}

const CLIENT_VERSION_HEADER_MAX_LEN: usize = 64;

fn sanitize_client_version(raw: &str) -> Option<String> {
    let sanitized: String = raw
        .trim()
        .chars()
        .filter(|c| matches!(c, '0'..='9' | 'A'..='Z' | 'a'..='z' | '.' | '_' | '+' | '-'))
        .take(CLIENT_VERSION_HEADER_MAX_LEN)
        .collect();

    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

fn build_backend_reqwest_client() -> Result<Client> {
    let mut default_headers = HeaderMap::new();
    if let Some(version) = sanitize_client_version(env!("CARGO_PKG_VERSION")) {
        default_headers.insert(
            HeaderName::from_static("x-core-version"),
            HeaderValue::from_str(&version).context("invalid x-core-version header value")?,
        );
    }

    // Force rustls for consistent cross-platform TLS behavior.
    Client::builder()
        .default_headers(default_headers)
        .use_rustls_tls()
        .http1_only()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))
}

fn parse_api_response_json(text: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(text).with_context(|| format!("parse API JSON: {text}"))?;
    let Some(obj) = v.as_object() else {
        return Ok(v);
    };
    if let Some(success) = obj.get("success").and_then(|x| x.as_bool()) {
        if !success {
            let msg = obj
                .get("message")
                .or_else(|| obj.get("error"))
                .and_then(|x| x.as_str())
                .unwrap_or("request unsuccessful");
            anyhow::bail!("API request failed: {msg}");
        }
        if let Some(data) = obj.get("data") {
            if !data.is_null() {
                return Ok(data.clone());
            }
        }
        if let Some(user) = obj.get("user") {
            if !user.is_null() {
                return Ok(user.clone());
            }
        }
        let mut m = obj.clone();
        m.remove("success");
        return Ok(Value::Object(m));
    }
    Ok(v)
}

fn user_id_from_object(obj: &serde_json::Map<String, Value>) -> Option<String> {
    for key in ["id", "_id", "userId"] {
        if let Some(s) = obj.get(key).and_then(|x| x.as_str()) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Best-effort extraction of a user ID from an authenticated profile payload.
///
/// This function handles various envelope formats, including raw user objects
/// or those nested under `data` or `user` keys.
pub fn user_id_from_profile_payload(payload: &Value) -> Option<String> {
    let obj = payload.as_object()?;
    if let Some(data) = obj.get("data").and_then(|v| v.as_object()) {
        return user_id_from_object(data).or_else(|| {
            data.get("user")
                .and_then(|u| u.as_object())
                .and_then(user_id_from_object)
        });
    }

    user_id_from_object(obj).or_else(|| {
        obj.get("user")
            .and_then(|u| u.as_object())
            .and_then(user_id_from_object)
    })
}

/// Alias for [`user_id_from_profile_payload`] for semantic clarity in auth flows.
pub fn user_id_from_auth_me_payload(payload: &Value) -> Option<String> {
    user_id_from_profile_payload(payload)
}

#[derive(Debug, Clone, Deserialize)]
struct LoginTokenConsumeEnvelope {
    success: bool,
    data: LoginTokenConsumeData,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginTokenConsumeData {
    jwt_token: String,
}

/// A client for interacting with the TinyHumans / AlphaHuman backend API.
#[derive(Clone)]
pub struct BackendOAuthClient {
    client: Client,
    base: Url,
}

impl BackendOAuthClient {
    /// Creates a new `BackendOAuthClient` with the given API base URL.
    ///
    /// Any path, query, or fragment in `api_base` is stripped so that
    /// `Url::join` always resolves root-relative REST paths correctly.
    /// This guards against callers who pass a full LLM completions URL
    /// (e.g. `https://host/v1/chat/completions`) instead of just the origin:
    /// without stripping, `join("teams/me/usage")` would produce the wrong
    /// path `/v1/chat/teams/me/usage` via RFC 3986 relative resolution.
    pub fn new(api_base: &str) -> Result<Self> {
        let mut base = Url::parse(api_base.trim()).context("Invalid API base URL")?;
        anyhow::ensure!(
            matches!(base.scheme(), "http" | "https") && base.host_str().is_some(),
            "API base URL must be an absolute http(s) URL with host"
        );
        base.set_path("");
        base.set_query(None);
        base.set_fragment(None);
        let client = build_backend_reqwest_client()?;
        Ok(Self { client, base })
    }

    /// Borrow the underlying `reqwest::Client` for callers that need to
    /// drive a non-JSON request shape (e.g. `multipart/form-data` uploads
    /// for cloud STT) without re-implementing TLS/proxy plumbing.
    pub fn raw_client(&self) -> &Client {
        &self.client
    }

    /// Resolve a backend-relative path against the configured base URL.
    /// Mirrors what `authed_json` does internally so callers using
    /// `raw_client()` don't have to assemble URLs by hand.
    pub fn url_for(&self, path: &str) -> Result<Url> {
        self.base
            .join(path.trim_start_matches('/'))
            .with_context(|| format!("build URL for {path}"))
    }

    /// Returns the URL for initiating a login flow for a specific provider.
    pub fn login_url(&self, provider: &str) -> Result<Url> {
        let p = provider.trim().trim_matches('/');
        anyhow::ensure!(!p.is_empty(), "provider is required");
        self.base
            .join(&format!("auth/{p}/login"))
            .context("build login URL")
    }

    /// Fetches the current authenticated user profile using the provided JWT.
    pub async fn fetch_current_user(&self, bearer_jwt: &str) -> Result<Value> {
        let url = self.base.join("auth/me").context("build /auth/me URL")?;
        let resp = self
            .client
            .get(url)
            .header(AUTHORIZATION, bearer_authorization_value(bearer_jwt))
            .send()
            .await
            .context("GET /auth/me")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("GET /auth/me failed ({status}): {text}");
        }
        parse_api_response_json(&text)
    }

    /// Exchanges a one-time login token (e.g. from Telegram) for a long-lived JWT.
    pub async fn consume_login_token(&self, login_token: &str) -> Result<String> {
        let token = login_token.trim();
        anyhow::ensure!(!token.is_empty(), "login token is required");

        let url = self
            .base
            .join(&format!(
                "telegram/login-tokens/{}/consume",
                urlencoding::encode(token)
            ))
            .context("build login-token consume URL")?;

        let resp = self
            .client
            .post(url)
            .send()
            .await
            .context("consume login token")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("consume login token failed ({status}): {text}");
        }

        let env: LoginTokenConsumeEnvelope = serde_json::from_str(&text)
            .with_context(|| format!("parse consume-login-token JSON: {text}"))?;
        if !env.success {
            anyhow::bail!("consume login token unsuccessful: {text}");
        }

        let jwt = env.data.jwt_token.trim().to_string();
        anyhow::ensure!(
            !jwt.is_empty(),
            "consume login token response missing jwtToken"
        );
        Ok(jwt)
    }

    /// Creates a short-lived link token for connecting a specific communication channel.
    pub async fn create_channel_link_token(
        &self,
        channel: &str,
        bearer_jwt: &str,
    ) -> Result<Value> {
        let channel = channel.trim().trim_matches('/');
        anyhow::ensure!(!channel.is_empty(), "channel is required");
        let encoded_channel = urlencoding::encode(channel);

        let url = self
            .base
            .join(&format!("auth/channels/{encoded_channel}/link-token"))
            .context("build channel link-token URL")?;

        let resp = self
            .client
            .post(url)
            .header(AUTHORIZATION, bearer_authorization_value(bearer_jwt))
            .send()
            .await
            .context("create channel link token")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("create channel link token failed ({status}): {text}");
        }

        parse_api_response_json(&text)
    }

    /// Generic authenticated JSON request helper for backend API routes.
    pub async fn authed_json(
        &self,
        bearer_jwt: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let url = self
            .base
            .join(path.trim_start_matches('/'))
            .with_context(|| format!("build URL for {path}"))?;

        let mut request = self
            .client
            .request(method.clone(), url.clone())
            .header(AUTHORIZATION, bearer_authorization_value(bearer_jwt));

        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request.send().await.map_err(|e| {
            // Walk the error source chain so transient markers hidden in nested
            // causes (reqwest -> hyper -> rustls TLS EOF, etc.) still classify
            // correctly. The top-level `e.to_string()` often only carries the
            // outermost wrapper, e.g. "error sending request for url (...)".
            let mut error_message = e.to_string();
            let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&e);
            while let Some(s) = src {
                error_message.push_str(" → ");
                error_message.push_str(&s.to_string());
                src = s.source();
            }
            if crate::core::observability::contains_transient_transport_phrase(&error_message) {
                tracing::warn!(
                    domain = "backend_api",
                    operation = "authed_json",
                    method = method.as_str(),
                    path = url.path(),
                    failure = "transport",
                    error = %error_message,
                    "[backend_api] transient transport failure on {} {}: {}",
                    method.as_str(),
                    url.path(),
                    error_message,
                );
            } else {
                crate::core::observability::report_error(
                    error_message.as_str(),
                    "backend_api",
                    "authed_json",
                    &[
                        ("method", method.as_str()),
                        ("path", url.path()),
                        ("failure", "transport"),
                    ],
                );
            }
            anyhow::Error::new(e).context(format!(
                "backend request {} {}",
                method.as_str(),
                url.path()
            ))
        })?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let status_code = status.as_u16();
            let status_str = status_code.to_string();

            // 404 on `/channels/<provider>/messages/<id>` is an expected
            // state (user deleted the message provider-side, or backend
            // GC'd the relay row) — not a code bug. Surface a typed
            // `BackendApiError::MessageNotFound` so callers (`bus.rs`
            // streaming/thinking/delete/final paths) can clear stale
            // ids and skip retry, without funneling the 404 into
            // `report_error`. Targets `OPENHUMAN-TAURI-2Y` (~454 events).
            if status_code == 404 {
                if let Some((provider, message_id)) = parse_message_path(url.path()) {
                    tracing::info!(
                        domain = "backend_api",
                        operation = "authed_json",
                        provider = provider,
                        message_id = message_id,
                        "[backend_api] message-not-found 404 on {} {} — surfacing typed error",
                        method.as_str(),
                        url.path(),
                    );
                    return Err(anyhow::Error::new(BackendApiError::MessageNotFound {
                        provider: provider.to_string(),
                        message_id: message_id.to_string(),
                    }));
                }
            }

            // These are transient infrastructure errors (proxy/CDN/backend
            // temporarily unavailable). They are not code bugs and callers already
            // implement retry/disable logic, so skip Sentry to avoid noise.
            let is_transient_infra =
                crate::core::observability::is_transient_http_status_code(status_code);
            let is_budget_exhausted = status_code == 400
                && crate::openhuman::inference::provider::is_budget_exhausted_message(&text);
            if is_budget_exhausted {
                tracing::info!(
                    method = method.as_str(),
                    path = url.path(),
                    status = status_code,
                    failure = "non_2xx",
                    kind = "budget",
                    "[backend_api] budget-exhausted 400 on {} {} — not reporting to Sentry",
                    method.as_str(),
                    url.path(),
                );
            } else if is_transient_infra {
                tracing::warn!(
                    domain = "backend_api",
                    operation = "authed_json",
                    method = method.as_str(),
                    path = url.path(),
                    status = status_code,
                    failure = "non_2xx",
                    "[backend_api] transient {status} on {} {} — not reporting to Sentry",
                    method.as_str(),
                    url.path(),
                );
            } else {
                crate::core::observability::report_error(
                    format!(
                        "{} {} failed ({status}); response_body_len={}",
                        method.as_str(),
                        url.path(),
                        text.len()
                    )
                    .as_str(),
                    "backend_api",
                    "authed_json",
                    &[
                        ("method", method.as_str()),
                        ("path", url.path()),
                        ("status", status_str.as_str()),
                        ("failure", "non_2xx"),
                    ],
                );
            }
            anyhow::bail!(
                "{} {} failed ({status}): {text}",
                method.as_str(),
                url.path()
            );
        }

        parse_api_response_json(&text)
    }

    /// Sends a message to a communication channel.
    pub async fn send_channel_message(
        &self,
        channel: &str,
        bearer_jwt: &str,
        message_body: Value,
    ) -> Result<Value> {
        let channel = channel.trim().trim_matches('/');
        anyhow::ensure!(!channel.is_empty(), "channel is required");
        let encoded = urlencoding::encode(channel);
        self.authed_json(
            bearer_jwt,
            Method::POST,
            &format!("channels/{encoded}/messages"),
            Some(message_body),
        )
        .await
    }

    /// Signals "the agent is typing…" on a channel that supports it
    /// (Telegram's `sendChatAction`, Slack's typing event, …). The backend
    /// resolves the target chat from the channel integration metadata and
    /// is responsible for hitting the provider-native API.
    ///
    /// Telegram keeps the typing indicator alive for ~5 seconds per call,
    /// so callers should re-invoke every ~4 s for as long as the turn is
    /// in flight. Returns `Err` if the backend doesn't support typing for
    /// this channel — caller should swallow the error silently.
    pub async fn send_channel_typing(&self, channel: &str, bearer_jwt: &str) -> Result<Value> {
        let channel = channel.trim().trim_matches('/');
        anyhow::ensure!(!channel.is_empty(), "channel is required");
        let encoded = urlencoding::encode(channel);
        self.authed_json(
            bearer_jwt,
            Method::POST,
            &format!("channels/{encoded}/typing"),
            Some(json!({})),
        )
        .await
    }

    /// Edits an existing channel message. Used by the progressive-edit
    /// streaming path (Telegram / Slack) to coalesce live deltas into a
    /// single evolving outbound message rather than spamming the chat
    /// with one bubble per token.
    ///
    /// `message_id` is the backend-returned id of the message that was
    /// first sent via [`Self::send_channel_message`]. Returns the
    /// updated message record, or an `Err` if the backend does not
    /// support editing for this channel (caller should fall back to
    /// atomic-final delivery).
    pub async fn send_channel_edit(
        &self,
        channel: &str,
        message_id: &str,
        bearer_jwt: &str,
        edit_body: Value,
    ) -> Result<Value> {
        let channel = channel.trim().trim_matches('/');
        anyhow::ensure!(!channel.is_empty(), "channel is required");
        anyhow::ensure!(!message_id.is_empty(), "message_id is required");
        let encoded_channel = urlencoding::encode(channel);
        let encoded_id = urlencoding::encode(message_id);
        self.authed_json(
            bearer_jwt,
            Method::PATCH,
            &format!("channels/{encoded_channel}/messages/{encoded_id}"),
            Some(edit_body),
        )
        .await
    }

    /// Deletes a message from a communication channel. Used to clean up
    /// ephemeral messages (e.g. thinking indicators) after the final
    /// response has been delivered.
    pub async fn send_channel_delete(
        &self,
        channel: &str,
        message_id: &str,
        bearer_jwt: &str,
    ) -> Result<Value> {
        let channel = channel.trim().trim_matches('/');
        anyhow::ensure!(!channel.is_empty(), "channel is required");
        anyhow::ensure!(!message_id.is_empty(), "message_id is required");
        let encoded_channel = urlencoding::encode(channel);
        let encoded_id = urlencoding::encode(message_id);
        self.authed_json(
            bearer_jwt,
            Method::DELETE,
            &format!("channels/{encoded_channel}/messages/{encoded_id}"),
            None,
        )
        .await
    }

    /// Sends a reaction (e.g. emoji) to a message in a channel.
    pub async fn send_channel_reaction(
        &self,
        channel: &str,
        bearer_jwt: &str,
        reaction_body: Value,
    ) -> Result<Value> {
        let channel = channel.trim().trim_matches('/');
        anyhow::ensure!(!channel.is_empty(), "channel is required");
        let encoded = urlencoding::encode(channel);
        self.authed_json(
            bearer_jwt,
            Method::POST,
            &format!("channels/{encoded}/reactions"),
            Some(reaction_body),
        )
        .await
    }

    /// Creates a new thread in a communication channel.
    pub async fn create_channel_thread(
        &self,
        channel: &str,
        bearer_jwt: &str,
        title: &str,
    ) -> Result<Value> {
        let channel = channel.trim().trim_matches('/');
        anyhow::ensure!(!channel.is_empty(), "channel is required");
        anyhow::ensure!(!title.trim().is_empty(), "title is required");
        let encoded = urlencoding::encode(channel);
        let body = serde_json::json!({ "title": title.trim() });
        self.authed_json(
            bearer_jwt,
            Method::POST,
            &format!("channels/{encoded}/threads"),
            Some(body),
        )
        .await
    }

    /// Updates an existing thread (e.g., closing or reopening it).
    pub async fn update_channel_thread(
        &self,
        channel: &str,
        bearer_jwt: &str,
        thread_id: &str,
        action: &str,
    ) -> Result<Value> {
        let channel = channel.trim().trim_matches('/');
        anyhow::ensure!(!channel.is_empty(), "channel is required");
        anyhow::ensure!(!thread_id.trim().is_empty(), "threadId is required");
        anyhow::ensure!(
            action == "close" || action == "reopen",
            "action must be 'close' or 'reopen'"
        );
        let encoded_channel = urlencoding::encode(channel);
        let encoded_thread = urlencoding::encode(thread_id.trim());
        let body = serde_json::json!({ "action": action });
        self.authed_json(
            bearer_jwt,
            Method::PATCH,
            &format!("channels/{encoded_channel}/threads/{encoded_thread}"),
            Some(body),
        )
        .await
    }

    /// Lists threads in a communication channel, optionally filtering by status.
    pub async fn list_channel_threads(
        &self,
        channel: &str,
        bearer_jwt: &str,
        active_filter: Option<bool>,
    ) -> Result<Value> {
        let channel = channel.trim().trim_matches('/');
        anyhow::ensure!(!channel.is_empty(), "channel is required");
        let encoded = urlencoding::encode(channel);
        let mut path = format!("channels/{encoded}/threads");
        if let Some(active) = active_filter {
            path.push_str(if active {
                "?active=true"
            } else {
                "?active=false"
            });
        }
        self.authed_json(bearer_jwt, Method::GET, &path, None).await
    }
}

#[cfg(test)]
#[path = "rest_tests.rs"]
mod rest_tests;
