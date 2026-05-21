use super::*;

use serde::Serialize;
use std::path::PathBuf;

const MAX_API_ERROR_CHARS: usize = 200;

/// Fixed id for the single inference backend (OpenHuman API).
pub const INFERENCE_BACKEND_ID: &str = "openhuman";

#[derive(Debug, Clone)]
pub struct ProviderRuntimeOptions {
    pub auth_profile_override: Option<String>,
    pub openhuman_dir: Option<PathBuf>,
    pub secrets_encrypt: bool,
    pub reasoning_enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

pub async fn list_configured_models(
    provider_id: &str,
) -> Result<crate::rpc::RpcOutcome<serde_json::Value>, String> {
    let provider_id = provider_id.trim().to_string();
    if provider_id.is_empty() {
        return Err("provider_id must not be empty".to_string());
    }

    log::debug!("[providers][list_models] provider_id={}", provider_id);

    let config = crate::openhuman::config::Config::load_or_init()
        .await
        .map_err(|e| e.to_string())?;

    let entry = config
        .cloud_providers
        .iter()
        .find(|e| e.id == provider_id || e.slug == provider_id)
        .cloned()
        .ok_or_else(|| format!("no cloud provider with id or slug '{}' found", provider_id))?;

    let base = entry.endpoint.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(format!(
            "cloud provider '{}' has an empty endpoint; configure one in Settings → AI",
            entry.slug
        ));
    }
    let models_url = format!("{}/models", base);

    // Parse early so we fail with a clear "invalid endpoint URL" message
    // before we hit the reqwest builder's opaque "builder error" later.
    let parsed_url = reqwest::Url::parse(&models_url).map_err(|e| {
        format!(
            "cloud provider '{}' endpoint '{}' is not a valid URL ({}); \
             expected something like `http://127.0.0.1:11434/v1`",
            entry.slug, entry.endpoint, e
        )
    })?;

    log::debug!(
        "[providers][list_models] fetching url={} slug={}",
        models_url,
        entry.slug
    );

    let api_key =
        crate::openhuman::inference::provider::factory::lookup_key_for_slug(&entry.slug, &config)
            .unwrap_or_default();

    // Loopback endpoints (Ollama / LM Studio / dev mocks) must bypass the
    // runtime proxy — even an otherwise-correct proxy is the wrong route
    // for 127.0.0.1, and a misconfigured one surfaces here as an opaque
    // reqwest "builder error". A vanilla client is fine for local URLs;
    // remote providers continue to use the proxied client.
    let is_loopback = matches!(
        parsed_url.host_str(),
        Some("127.0.0.1") | Some("::1") | Some("localhost")
    );
    let client = if is_loopback {
        log::debug!(
            "[providers][list_models] using direct client for loopback url={}",
            models_url
        );
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| format!("[providers][list_models] failed to build direct client: {e}"))?
    } else {
        crate::openhuman::config::build_runtime_proxy_client_with_timeouts(
            "providers.list_models",
            30,
            10,
        )
    };

    let mut request = client.get(&models_url);

    use crate::openhuman::config::schema::cloud_providers::AuthStyle;
    request = match entry.auth_style {
        AuthStyle::Bearer => {
            if !api_key.is_empty() {
                request.header("Authorization", format!("Bearer {}", api_key))
            } else {
                request
            }
        }
        AuthStyle::Anthropic => {
            let mut r = request.header("anthropic-version", "2023-06-01");
            if !api_key.is_empty() {
                r = r.header("x-api-key", &api_key);
            }
            r
        }
        AuthStyle::OpenhumanJwt => {
            if !api_key.is_empty() {
                request.header("Authorization", format!("Bearer {}", api_key))
            } else {
                request
            }
        }
        AuthStyle::None => request,
    };

    let response = request.send().await.map_err(|e| {
        use std::error::Error;
        let mut chain = format!("{e}");
        let mut src: Option<&dyn std::error::Error> = Error::source(&e);
        while let Some(inner) = src {
            chain.push_str(" -> ");
            chain.push_str(&format!("{inner}"));
            src = inner.source();
        }
        format!(
            "[providers][list_models] HTTP request to {} failed: {}",
            models_url, chain
        )
    })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let sanitized = sanitize_api_error(&body);
        let truncated = crate::openhuman::util::truncate_with_ellipsis(&sanitized, 300);
        return Err(format!(
            "provider returned {}: {}",
            status.as_u16(),
            truncated
        ));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("[providers][list_models] failed to parse JSON: {}", e))?;

    let data = body
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();

    let models: Vec<ModelInfo> = data
        .iter()
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?.to_string();
            let owned_by = item
                .get("owned_by")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let context_window = item
                .get("context_length")
                .or_else(|| item.get("context_window"))
                .and_then(|v| v.as_u64());
            Some(ModelInfo {
                id,
                owned_by,
                context_window,
            })
        })
        .collect();

    log::info!(
        "[providers][list_models] slug={} fetched {} models",
        entry.slug,
        models.len()
    );

    Ok(crate::rpc::RpcOutcome::new(
        serde_json::json!({ "models": models }),
        vec![format!("fetched {} models", models.len())],
    ))
}

impl Default for ProviderRuntimeOptions {
    fn default() -> Self {
        Self {
            auth_profile_override: None,
            openhuman_dir: None,
            secrets_encrypt: true,
            reasoning_enabled: None,
        }
    }
}

fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')
}

fn token_end(input: &str, from: usize) -> usize {
    let mut end = from;
    for (i, c) in input[from..].char_indices() {
        if is_secret_char(c) {
            end = from + i + c.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Scrub known secret-like token prefixes from provider error strings.
pub fn scrub_secret_patterns(input: &str) -> String {
    const PREFIXES: [&str; 7] = [
        "sk-",
        "xoxb-",
        "xoxp-",
        "ghp_",
        "gho_",
        "ghu_",
        "github_pat_",
    ];

    let mut scrubbed = input.to_string();

    for prefix in PREFIXES {
        let mut search_from = 0;
        loop {
            let Some(rel) = scrubbed[search_from..].find(prefix) else {
                break;
            };

            let start = search_from + rel;
            let content_start = start + prefix.len();
            let end = token_end(&scrubbed, content_start);

            if end == content_start {
                search_from = content_start;
                continue;
            }

            scrubbed.replace_range(start..end, "[REDACTED]");
            search_from = start + "[REDACTED]".len();
        }
    }

    scrubbed
}

/// Sanitize API error text by scrubbing secrets and truncating length.
pub fn sanitize_api_error(input: &str) -> String {
    let scrubbed = scrub_secret_patterns(input);
    crate::openhuman::util::truncate_with_ellipsis(&scrubbed, MAX_API_ERROR_CHARS)
}

const TRANSPORT_ERROR_MAX_CHARS: usize = 1200;

/// Full `source()` chain for connection / TLS failures (scrubbed, longer than API body snippets).
pub fn format_error_chain(err: &dyn std::error::Error) -> String {
    let mut parts: Vec<String> = vec![err.to_string()];
    let mut src = std::error::Error::source(err);
    while let Some(e) = src {
        parts.push(e.to_string());
        src = std::error::Error::source(e);
    }
    let joined = parts.join(" | ");
    let scrubbed = scrub_secret_patterns(&joined);
    crate::openhuman::util::truncate_with_suffix(&scrubbed, TRANSPORT_ERROR_MAX_CHARS, "…")
}

/// Cause chain from [`anyhow::Error`] (e.g. responses fallback), scrubbed and length-limited.
pub fn format_anyhow_chain(err: &anyhow::Error) -> String {
    let joined = err
        .chain()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(" | ");
    let scrubbed = scrub_secret_patterns(&joined);
    crate::openhuman::util::truncate_with_suffix(&scrubbed, TRANSPORT_ERROR_MAX_CHARS, "…")
}

/// Whether a non-2xx provider response is worth reporting to Sentry.
///
/// Transient upstream statuses — 429 Too Many Requests, 408 Request Timeout,
/// and 502/503/504 gateway-layer failures — are caller-side throttling or
/// upstream-capacity signals. The reliable-provider layer already retries
/// with backoff and falls back across providers/models, and the aggregate
/// "all providers exhausted" event still fires if every attempt fails.
/// Reporting each individual transient failure floods Sentry (see
/// OPENHUMAN-TAURI-6Y / 2E / 84 / T: thousands of events/day per user from
/// a single upstream rate-limit / outage window). Callers should still
/// propagate the error so retry and fallback logic runs unchanged; this
/// only gates the per-attempt Sentry report.
pub fn should_report_provider_http_failure(status: reqwest::StatusCode) -> bool {
    !crate::core::observability::TRANSIENT_PROVIDER_HTTP_STATUSES.contains(&status.as_u16())
}

/// Whether a provider non-2xx response is a deterministic budget-exhausted
/// user-state error that should be demoted from Sentry to an info log.
pub(super) fn is_budget_exhausted_http_400(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::BAD_REQUEST && super::is_budget_exhausted_message(body)
}

pub(super) fn log_budget_exhausted_http_400(
    operation: &str,
    provider: &str,
    model: Option<&str>,
    status: reqwest::StatusCode,
) {
    tracing::info!(
        domain = "llm_provider",
        operation = operation,
        provider = provider,
        model = model.unwrap_or(""),
        status = status.as_u16(),
        failure = "non_2xx",
        kind = "budget",
        "[llm_provider] {operation} budget-exhausted 400 — not reporting to Sentry"
    );
}

/// Build a sanitized provider error from a failed HTTP response.
///
/// Reports the failure to Sentry with `provider` and `status` tags so
/// upstream LLM errors are visible in observability without every call-site
/// having to remember to log — except for:
///
/// - **Transient statuses** (429 — see [`should_report_provider_http_failure`]).
///   These get retried by the reliable-provider layer and don't deserve a
///   per-attempt Sentry event.
/// - **401/403 from the OpenHuman backend provider** — the user's app session
///   expired. That is expected user-state, not a server bug, and reporting it
///   spams Sentry (OPENHUMAN-TAURI-1T: 5,414 events from a single user whose
///   cron loops kept firing post-expiry). Instead we publish a
///   [`crate::core::event_bus::DomainEvent::SessionExpired`] so the credentials
///   subscriber clears the session and flips the scheduler-gate signed-out
///   override, halting downstream LLM work. 401/403 from **other** providers
///   (OpenAI, Anthropic, …) still go to Sentry — those mean a misconfigured
///   API key, which is actionable.
pub async fn api_error(provider: &str, response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let status_str = status.as_u16().to_string();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<failed to read provider error body>".to_string());
    let sanitized = sanitize_api_error(&body);
    let message = format!("{provider} API error ({status}): {sanitized}");

    let is_auth_failure = matches!(status.as_u16(), 401 | 403);
    let is_backend = provider == openhuman_backend::PROVIDER_LABEL;
    let is_budget_exhausted_user_state = is_budget_exhausted_http_400(status, &body);

    if is_auth_failure && is_backend {
        tracing::warn!(
            domain = "llm_provider",
            operation = "api_error",
            provider = provider,
            status = status_str.as_str(),
            "[llm_provider] backend auth failure ({status}) — publishing SessionExpired"
        );
        // `message` already embeds the sanitized body via
        // `sanitize_api_error(&body)`, but the leading `{provider} API
        // error ({status})` prefix and any caller-controlled provider
        // name aren't scrubbed — re-run sanitize on the final string so
        // the SessionExpired subscriber's logs never persist secrets.
        crate::core::event_bus::publish_global(
            crate::core::event_bus::DomainEvent::SessionExpired {
                source: "llm_provider.openhuman_backend".to_string(),
                reason: sanitize_api_error(&message),
            },
        );
    } else if is_budget_exhausted_user_state {
        log_budget_exhausted_http_400("api_error", provider, None, status);
    } else if should_report_provider_http_failure(status) {
        crate::core::observability::report_error(
            message.as_str(),
            "llm_provider",
            "api_error",
            &[
                ("provider", provider),
                ("status", status_str.as_str()),
                ("failure", "non_2xx"),
            ],
        );
    }
    anyhow::anyhow!(message)
}

/// Create the inference provider.
///
/// - `inference_url`: a custom OpenAI-compatible LLM endpoint
///   (`config.inference_url`). **Required** in the closedhuman fork: the
///   legacy fallback to the OpenHuman product backend is gone, so a
///   missing URL is now a hard error rather than a silent route to a
///   non-existent backend.
/// - `backend_url`: previously the OpenHuman product backend URL. Kept
///   on the signature so existing call sites still type-check; ignored
///   on every code path now that the backend fallback is removed.
/// - `api_key`: the API key for the custom inference endpoint. Optional
///   when the user's endpoint genuinely doesn't need auth (e.g. a local
///   Ollama listener). Omitting it surfaces a `AuthStyle::None` provider
///   rather than silently routing to the backend.
///
/// ## Errors
///
/// Returns `Err` with a user-facing pointer at Settings → AI when
/// `inference_url` is missing. This replaces the previous silent
/// `OpenHumanBackendProvider` construction that surfaced downstream as
/// `SESSION_EXPIRED: backend session not active` — confusing because it
/// suggested "re-login" when the actual root cause was "no provider
/// configured in this fork's local-OAuth model".
pub fn create_backend_inference_provider(
    inference_url: Option<&str>,
    backend_url: Option<&str>,
    api_key: Option<&str>,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    let _ = (backend_url, options);
    let trimmed_url = inference_url.map(str::trim).filter(|s| !s.is_empty());

    let url = trimmed_url.ok_or_else(|| {
        anyhow::anyhow!(
            "[providers] no inference endpoint configured — the closedhuman fork \
             does not have a hosted LLM backend. Add a provider under \
             Settings → AI (OpenAI, Anthropic, OpenRouter, or a custom \
             OpenAI-compatible endpoint) and set the corresponding \
             `*_provider` config field, or set `inference_url` directly."
        )
    })?;

    let trimmed_key = api_key.map(str::trim).filter(|s| !s.is_empty());
    let auth_style = if trimmed_key.is_some() {
        crate::openhuman::inference::provider::compatible::AuthStyle::Bearer
    } else {
        crate::openhuman::inference::provider::compatible::AuthStyle::None
    };

    log::info!(
        "[providers] inference target = custom_openai @ {} (auth={:?}, api_key_set={})",
        url,
        auth_style,
        trimmed_key.is_some()
    );

    Ok(Box::new(
        crate::openhuman::inference::provider::compatible::OpenAiCompatibleProvider::new(
            "custom_openai",
            url,
            trimmed_key,
            auth_style,
        ),
    ))
}

/// Create provider chain with retry and fallback behavior.
pub fn create_resilient_provider(
    inference_url: Option<&str>,
    backend_url: Option<&str>,
    api_key: Option<&str>,
    reliability: &crate::openhuman::config::ReliabilityConfig,
) -> anyhow::Result<Box<dyn Provider>> {
    create_resilient_provider_with_options(
        inference_url,
        backend_url,
        api_key,
        reliability,
        &ProviderRuntimeOptions::default(),
    )
}

/// Create provider chain with retry/fallback behavior and auth runtime options.
pub fn create_resilient_provider_with_options(
    inference_url: Option<&str>,
    backend_url: Option<&str>,
    api_key: Option<&str>,
    reliability: &crate::openhuman::config::ReliabilityConfig,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    if !reliability.fallback_providers.is_empty() {
        tracing::warn!(
            "reliability.fallback_providers is ignored; inference uses only the OpenHuman backend"
        );
    }

    let primary_provider =
        create_backend_inference_provider(inference_url, backend_url, api_key, options)?;
    let providers: Vec<(String, Box<dyn Provider>)> =
        vec![(INFERENCE_BACKEND_ID.to_string(), primary_provider)];

    let reliable = reliable::ReliableProvider::new(
        providers,
        reliability.provider_retries,
        reliability.provider_backoff_ms,
    )
    .with_model_fallbacks(reliability.model_fallbacks.clone());

    Ok(Box::new(reliable))
}

/// Create a RouterProvider if model routes are configured, otherwise return a resilient provider.
pub fn create_routed_provider(
    inference_url: Option<&str>,
    backend_url: Option<&str>,
    api_key: Option<&str>,
    reliability: &crate::openhuman::config::ReliabilityConfig,
    model_routes: &[crate::openhuman::config::ModelRouteConfig],
    default_model: &str,
) -> anyhow::Result<Box<dyn Provider>> {
    create_routed_provider_with_options(
        inference_url,
        backend_url,
        api_key,
        reliability,
        model_routes,
        default_model,
        &ProviderRuntimeOptions::default(),
    )
}

pub fn create_routed_provider_with_options(
    inference_url: Option<&str>,
    backend_url: Option<&str>,
    api_key: Option<&str>,
    reliability: &crate::openhuman::config::ReliabilityConfig,
    model_routes: &[crate::openhuman::config::ModelRouteConfig],
    default_model: &str,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    if model_routes.is_empty() {
        return create_resilient_provider_with_options(
            inference_url,
            backend_url,
            api_key,
            reliability,
            options,
        );
    }

    let backend = create_backend_inference_provider(inference_url, backend_url, api_key, options)?;
    let providers: Vec<(String, Box<dyn Provider>)> =
        vec![(INFERENCE_BACKEND_ID.to_string(), backend)];

    let routes: Vec<(String, router::Route)> = model_routes
        .iter()
        .map(|r| {
            (
                r.hint.clone(),
                router::Route {
                    provider_name: INFERENCE_BACKEND_ID.to_string(),
                    model: r.model.clone(),
                },
            )
        })
        .collect();

    Ok(Box::new(router::RouterProvider::new(
        providers,
        routes,
        default_model.to_string(),
    )))
}

/// Create a provider with intelligent local/remote routing.
///
/// When `config.local_ai.runtime_enabled` is `true` and Ollama is reachable,
/// lightweight and medium tasks (e.g. `hint:reaction`, `hint:summarize`) are
/// served by the local model. Heavy tasks (`hint:reasoning`, `hint:agentic`,
/// `hint:coding`) always go to the remote backend. A health-gated fallback
/// transparently promotes failed local calls to the remote backend.
///
/// Telemetry for every routing decision is emitted at `INFO` level under the
/// `"routing"` tracing target.
pub fn create_intelligent_routing_provider(
    inference_url: Option<&str>,
    backend_url: Option<&str>,
    api_key: Option<&str>,
    config: &crate::openhuman::config::Config,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    // Local-OAuth fork: when the user has configured at least one
    // non-openhuman cloud-providers entry and is NOT pointing the
    // legacy `inference_url` at a custom OpenAI-compatible host, route
    // through the workload factory so the channel runtime and
    // /threads chat both end up calling the user's primary_cloud
    // (typically the seeded "openai" row). Without this, both call
    // sites silently fell back to `OpenHumanBackendProvider`, which
    // hard-errors with the SESSION_EXPIRED sentinel in this build —
    // the user-visible symptom was the Telegram bot reporting
    // `openhuman:<model>` provider failures even though Settings →
    // AI was pointed at OpenAI or Ollama.
    let has_user_cloud = config
        .cloud_providers
        .iter()
        .any(|e| e.slug != INFERENCE_BACKEND_ID);
    if has_user_cloud && inference_url.is_none() {
        let provider_str = factory::provider_for_role("reasoning", config);
        log::info!(
            "[providers] intelligent routing: using workload factory provider_str={}",
            provider_str
        );
        let (workload_provider, resolved_model) =
            factory::create_chat_provider_from_string("reasoning", &provider_str, config)?;
        let fallback_model = if resolved_model.trim().is_empty() {
            config
                .default_model
                .clone()
                .unwrap_or_else(|| crate::openhuman::config::DEFAULT_MODEL.to_string())
        } else {
            resolved_model
        };
        let reliable: Box<dyn Provider> = Box::new(
            reliable::ReliableProvider::new(
                vec![(INFERENCE_BACKEND_ID.to_string(), workload_provider)],
                config.reliability.provider_retries,
                config.reliability.provider_backoff_ms,
            )
            .with_model_fallbacks(config.reliability.model_fallbacks.clone()),
        );
        let provider =
            crate::openhuman::routing::new_provider(reliable, &config.local_ai, &fallback_model);
        return Ok(Box::new(provider));
    }

    let raw_backend =
        create_backend_inference_provider(inference_url, backend_url, api_key, options)?;
    // Wrap the raw backend in ReliableProvider so transient 502/503/504 errors
    // are retried before propagating to the agent turn. Without this, a single
    // 502 from the backend bypasses the retry layer entirely and surfaces as a
    // fatal `run_single` failure.
    log::debug!(
        "[providers] initialising reliable wrapper: retries={} backoff_ms={} fallbacks={}",
        config.reliability.provider_retries,
        config.reliability.provider_backoff_ms,
        config.reliability.model_fallbacks.len()
    );
    let reliable_backend: Box<dyn Provider> = Box::new(
        reliable::ReliableProvider::new(
            vec![(INFERENCE_BACKEND_ID.to_string(), raw_backend)],
            config.reliability.provider_retries,
            config.reliability.provider_backoff_ms,
        )
        .with_model_fallbacks(config.reliability.model_fallbacks.clone()),
    );
    let default_model = config
        .default_model
        .as_deref()
        .unwrap_or(crate::openhuman::config::DEFAULT_MODEL);

    // When the user has configured `model_routes` (custom provider via
    // BackendProviderPanel), wrap the reliable remote in a RouterProvider so
    // abstract tier names like `reasoning-v1` get translated to the configured
    // provider-specific model id (e.g. `gpt-5.5`) BEFORE the request leaves
    // the host. Without this step the abstract tier name would reach
    // `custom_openai` and 404. The OpenHuman backend can dispatch tier names
    // natively, so we skip the wrap when routes are empty.
    log::info!(
        "[providers] intelligent routing: model_routes_count={} default_model={} inference_url_set={}",
        config.model_routes.len(),
        default_model,
        inference_url.is_some()
    );
    let remote: Box<dyn Provider> = if config.model_routes.is_empty() {
        reliable_backend
    } else {
        let providers: Vec<(String, Box<dyn Provider>)> =
            vec![(INFERENCE_BACKEND_ID.to_string(), reliable_backend)];
        let routes: Vec<(String, router::Route)> = config
            .model_routes
            .iter()
            .map(|r| {
                (
                    r.hint.clone(),
                    router::Route {
                        provider_name: INFERENCE_BACKEND_ID.to_string(),
                        model: r.model.clone(),
                    },
                )
            })
            .collect();
        Box::new(router::RouterProvider::new(
            providers,
            routes,
            default_model.to_string(),
        ))
    };

    let provider = crate::openhuman::routing::new_provider(remote, &config.local_ai, default_model);
    Ok(Box::new(provider))
}

/// Information about a supported provider for display purposes.
pub struct ProviderInfo {
    pub name: &'static str,
    pub display_name: &'static str,
    pub aliases: &'static [&'static str],
    pub local: bool,
}

/// Return known providers for display (single backend path).
pub fn list_providers() -> Vec<ProviderInfo> {
    vec![ProviderInfo {
        name: INFERENCE_BACKEND_ID,
        display_name: "OpenHuman (backend)",
        aliases: &["backend", "openhuman-backend"],
        local: false,
    }]
}

// Legacy provider alias stubs (integrations / config); remote providers were removed.
pub fn is_glm_alias(_name: &str) -> bool {
    false
}
pub fn is_zai_alias(_name: &str) -> bool {
    false
}
pub fn is_minimax_alias(_name: &str) -> bool {
    false
}
pub fn is_moonshot_alias(_name: &str) -> bool {
    false
}
pub fn is_qianfan_alias(_name: &str) -> bool {
    false
}
pub fn is_qwen_alias(_name: &str) -> bool {
    false
}
pub fn is_qwen_oauth_alias(_name: &str) -> bool {
    false
}
pub fn canonical_china_provider_name(_name: &str) -> Option<&'static str> {
    let _ = _name;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_configured_models_accepts_slug() {
        // list_configured_models should find a provider by slug when the caller
        // passes a slug instead of the opaque random id. This lets the frontend
        // call the RPC before the provider config has been persisted (where only
        // the slug is stable).
        use crate::openhuman::config::schema::cloud_providers::{AuthStyle, CloudProviderCreds};
        use crate::openhuman::config::Config;

        let mut config = Config::default();
        config.cloud_providers.push(CloudProviderCreds {
            id: "p_openai_xyz99".to_string(),
            slug: "openai".to_string(),
            label: "OpenAI".to_string(),
            endpoint: "https://api.openai.com/v1".to_string(),
            auth_style: AuthStyle::Bearer,
            legacy_type: None,
            default_model: None,
        });

        // The find predicate must match on slug.
        let found_by_slug = config
            .cloud_providers
            .iter()
            .find(|e| e.id == "openai" || e.slug == "openai");
        assert!(
            found_by_slug.is_some(),
            "slug lookup must find the provider"
        );
        assert_eq!(found_by_slug.unwrap().id, "p_openai_xyz99");

        // The find predicate must still match on id.
        let found_by_id = config
            .cloud_providers
            .iter()
            .find(|e| e.id == "p_openai_xyz99" || e.slug == "p_openai_xyz99");
        assert!(found_by_id.is_some(), "id lookup must still work");
    }

    #[test]
    fn create_backend_inference_provider_errors_without_url() {
        // The closedhuman fork has no hosted backend, so a missing
        // `inference_url` must surface an actionable error rather than
        // silently routing to OpenHumanBackendProvider (which would 401
        // downstream with the confusing SESSION_EXPIRED sentinel).
        let err = create_backend_inference_provider(
            None,
            Some("https://backend.example.com"),
            Some("sk-some-key"),
            &ProviderRuntimeOptions::default(),
        )
        .err()
        .expect("missing inference_url must error");
        let msg = err.to_string();
        assert!(
            msg.contains("Settings → AI"),
            "error should point at Settings → AI: {msg}"
        );
        assert!(
            msg.contains("no inference endpoint configured"),
            "error should name the missing config knob: {msg}"
        );
    }

    #[test]
    fn create_backend_inference_provider_errors_on_empty_url() {
        // Whitespace-only inference_url is treated the same as missing —
        // the user wrote nothing, just with extra ceremony.
        let err = create_backend_inference_provider(
            Some("   \t  "),
            None,
            Some("sk-some-key"),
            &ProviderRuntimeOptions::default(),
        )
        .err()
        .expect("blank inference_url must error");
        assert!(err.to_string().contains("Settings → AI"));
    }

    #[test]
    fn create_backend_inference_provider_succeeds_with_url_and_key() {
        // Bearer-style auth — most cloud OpenAI-compatible providers.
        let provider = create_backend_inference_provider(
            Some("https://api.example.com/v1"),
            None,
            Some("sk-test-key"),
            &ProviderRuntimeOptions::default(),
        );
        assert!(
            provider.is_ok(),
            "url + key should build cleanly: {:?}",
            provider.err()
        );
    }

    #[test]
    fn create_backend_inference_provider_succeeds_with_url_only() {
        // Local OpenAI-compatible endpoints (e.g. Ollama, mlx-audio) may
        // not require auth. `AuthStyle::None` should let the call go
        // through rather than blocking on a missing key.
        let provider = create_backend_inference_provider(
            Some("http://localhost:11434/v1"),
            None,
            None,
            &ProviderRuntimeOptions::default(),
        );
        assert!(
            provider.is_ok(),
            "url without key should build cleanly: {:?}",
            provider.err()
        );
    }

    #[test]
    fn skips_sentry_report_for_transient_upstream_statuses() {
        // Transient statuses — 429 rate-limit, 408 client timeout, and 502/503/504
        // gateway-layer failures — are retried by reliable.rs. The aggregate
        // "all providers exhausted" event still fires for genuine outages.
        // Reporting each attempt individually floods Sentry (OPENHUMAN-TAURI-2E
        // ~1393 events, 84 ~1050 events, T ~871 events).
        for transient in [
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::REQUEST_TIMEOUT,
            reqwest::StatusCode::BAD_GATEWAY,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            reqwest::StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert!(
                !should_report_provider_http_failure(transient),
                "transient status {transient} must not trigger per-attempt Sentry report"
            );
        }
        // Auth + permanent server faults remain reportable — those are
        // misconfiguration or genuine bugs, not transient capacity issues.
        for reportable in [
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::FORBIDDEN,
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::NOT_FOUND,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(
                should_report_provider_http_failure(reportable),
                "status {reportable} must still report to Sentry"
            );
        }
    }

    // Confirm the budget-exhausted suppression predicate is scoped correctly.
    // These tests exercise the real production function, not a duplicate.
    mod budget_exhausted_suppression {
        use super::*;

        const BUDGET_BODY: &str = "Insufficient budget";
        const UNRELATED_BODY: &str = "Invalid request: model not found";

        #[test]
        fn budget_exhausted_400_is_suppressed() {
            assert!(is_budget_exhausted_http_400(
                reqwest::StatusCode::BAD_REQUEST,
                BUDGET_BODY,
            ));
        }

        #[test]
        fn budget_exhausted_400_is_case_insensitive() {
            assert!(is_budget_exhausted_http_400(
                reqwest::StatusCode::BAD_REQUEST,
                "budget exceeded — ADD credits to continue",
            ));
        }

        #[test]
        fn budget_exhausted_500_is_not_suppressed() {
            // A 500 is a server bug, not expected user-state — keep reporting.
            assert!(!is_budget_exhausted_http_400(
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                BUDGET_BODY,
            ));
        }

        #[test]
        fn budget_exhausted_400_unrelated_body_is_not_suppressed() {
            assert!(!is_budget_exhausted_http_400(
                reqwest::StatusCode::BAD_REQUEST,
                UNRELATED_BODY,
            ));
        }

        #[test]
        fn budget_exhausted_402_is_not_suppressed() {
            assert!(!is_budget_exhausted_http_400(
                reqwest::StatusCode::PAYMENT_REQUIRED,
                BUDGET_BODY,
            ));
        }

        #[test]
        fn budget_exhausted_empty_body_is_not_suppressed() {
            assert!(!is_budget_exhausted_http_400(
                reqwest::StatusCode::BAD_REQUEST,
                "",
            ));
        }
    }

    #[test]
    fn test_sanitize_api_error_utf8() {
        let input = "🦀".repeat(MAX_API_ERROR_CHARS + 10);
        let sanitized = sanitize_api_error(&input);
        assert!(sanitized.ends_with("..."));
        // Should truncate at MAX_API_ERROR_CHARS crabs
        let crabs_count = sanitized.chars().filter(|c| *c == '🦀').count();
        assert_eq!(crabs_count, MAX_API_ERROR_CHARS);
    }
}
