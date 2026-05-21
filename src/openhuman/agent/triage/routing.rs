//! Local-vs-remote provider resolver for triage turns.
//!
//! ## What this does
//!
//! [`resolve_provider`] always builds the remote provider. Local AI is never
//! used for chat triage — the local path has been removed to guarantee that
//! a triage turn never errors due to Ollama unavailability.
//!
//! `ResolvedProvider.used_local` is preserved for telemetry compatibility but
//! is always `false`.
//!
//! ## Backend wiring
//!
//! In the closedhuman fork the legacy `create_routed_provider_with_options`
//! path hard-errors on a missing OpenHuman backend, so [`build_remote_provider`]
//! now routes through the workload factory's `chat` role (the triage agent
//! is a lightweight JSON classifier — same workload shape as a chat turn).
//! The factory resolves to whichever `cloud_providers` row the user has
//! marked primary, or to local Ollama if the user set `chat_provider =
//! "ollama:<model>"`.

use std::sync::Arc;

use anyhow::Context;

use crate::openhuman::config::Config;
use crate::openhuman::inference::provider::factory::{
    create_chat_provider_from_string, provider_for_role, PROVIDER_OPENHUMAN,
};
use crate::openhuman::inference::provider::Provider;

/// The concrete provider + metadata that [`crate::openhuman::agent::triage::evaluator::run_triage`]
/// should use for this particular triage turn.
pub struct ResolvedProvider {
    /// Ready-to-use provider, already constructed.
    pub provider: Arc<dyn Provider>,
    /// Provider name token — always `"openhuman"` (remote backend).
    /// Kept for telemetry / observability compat with the previous two-path design.
    pub provider_name: String,
    /// Model identifier — the concrete string `run_tool_call_loop`
    /// will hand to the provider.
    pub model: String,
    /// Always `false` — local AI is never used for triage.
    /// Preserved so existing telemetry subscribers that read this field do not
    /// need code changes.
    pub used_local: bool,
}

// ── Public API ──────────────────────────────────────────────────────────

/// Resolve a provider for a single triage turn. Always returns the remote
/// backend — local AI is hard-disabled for the chat/triage path.
pub async fn resolve_provider() -> anyhow::Result<ResolvedProvider> {
    let config = Config::load_or_init()
        .await
        .context("loading config for triage provider resolution")?;
    resolve_provider_with_config(&config).await
}

/// Inner half of [`resolve_provider`] that takes an already-loaded
/// [`Config`]. Exposed for tests and for the evaluator's retry path.
pub async fn resolve_provider_with_config(config: &Config) -> anyhow::Result<ResolvedProvider> {
    tracing::debug!(
        runtime_enabled = config.local_ai.runtime_enabled,
        "[triage::routing] resolving provider (always remote)"
    );
    build_remote_provider(config)
}

/// Build the local-arm provider for the tiered fallback chain (issue
/// #1257). Returns `None` when local AI is disabled or no chat model
/// is configured — callers (`evaluator::run_triage`) skip straight to
/// `Deferred` in that case.
///
/// The returned provider is a thin `OpenAiCompatibleProvider` pointed
/// at the configured local inference base (Ollama by default,
/// overridable via `OPENHUMAN_LOCAL_INFERENCE_URL`). It mirrors the
/// wiring `routing::factory::new_provider` uses for the local arm of
/// `IntelligentRoutingProvider` so the same model that serves
/// lightweight chat also serves the triage fallback.
pub fn build_local_provider_with_config(config: &Config) -> Option<ResolvedProvider> {
    use crate::openhuman::inference::provider::compatible::{AuthStyle, OpenAiCompatibleProvider};

    let local_cfg = &config.local_ai;
    if !local_cfg.runtime_enabled {
        tracing::debug!("[triage::routing] local arm disabled (runtime_enabled=false)");
        return None;
    }
    if local_cfg.chat_model_id.trim().is_empty() {
        tracing::debug!("[triage::routing] local arm skipped (no chat_model_id configured)");
        return None;
    }

    let override_base = std::env::var("OPENHUMAN_LOCAL_INFERENCE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty());
    let provider_kind = local_cfg.provider.trim().to_ascii_lowercase();
    let use_openai_compat = override_base.is_some()
        || matches!(
            provider_kind.as_str(),
            "llamacpp" | "llama-server" | "custom_openai"
        );

    let (label, base) = if use_openai_compat {
        let base = override_base
            .or_else(|| local_cfg.base_url.clone())
            .unwrap_or_else(|| "http://127.0.0.1:8080/v1".to_string());
        let label = if provider_kind == "custom_openai" {
            "custom_openai"
        } else {
            "llamacpp"
        };
        (label, base)
    } else {
        let ollama_base = crate::openhuman::inference::local::ollama_base_url();
        ("ollama", format!("{ollama_base}/v1"))
    };

    let local_api_key = local_cfg
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let auth_style = if local_api_key.is_some() {
        AuthStyle::Bearer
    } else {
        AuthStyle::None
    };
    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
        label,
        &base,
        local_api_key,
        auth_style,
    ));
    tracing::debug!(
        provider = %label,
        model = %local_cfg.chat_model_id,
        "[triage::routing] resolved local fallback provider"
    );
    Some(ResolvedProvider {
        provider,
        provider_name: label.to_string(),
        model: local_cfg.chat_model_id.clone(),
        used_local: true,
    })
}

// ── Provider builder ────────────────────────────────────────────────────

/// Build a provider for the triage turn via the workload factory's
/// `chat` role.
///
/// The legacy implementation here called
/// `provider::create_routed_provider_with_options` against
/// `config.inference_url` — that path constructs `OpenHumanBackendProvider`
/// when the URL is unset (default for the local-OAuth fork) and the
/// downstream `chat_with_system` then 401s with `SESSION_EXPIRED`. Every
/// inbound Composio trigger surfaced this as `[composio][triage]
/// run_triage failed ... error=resolving provider for triage turn`.
///
/// The workload factory takes the user's `chat_provider` (or falls
/// back to their primary `cloud_providers` row) and produces a
/// concrete `Box<dyn Provider>` + model string — same path the migrated
/// chat / memory-tree / channels surfaces use after commit `95f1e3c4`.
fn build_remote_provider(config: &Config) -> anyhow::Result<ResolvedProvider> {
    let resolved = provider_for_role("chat", config);
    if resolved == PROVIDER_OPENHUMAN {
        // The factory's `make_openhuman_backend` would hard-error
        // anyway, but pre-empting it lets us surface a clearer
        // actionable message that matches the rest of the fork's
        // "Settings → AI" pointers.
        anyhow::bail!(
            "no chat provider configured for triage — add a `cloud_providers` \
             entry (e.g. OpenAI) or set `chat_provider` to a `slug:model` (e.g. \
             `ollama:gpt-oss:20b`) under Settings → AI"
        );
    }
    let (provider_box, model) = create_chat_provider_from_string("chat", &resolved, config)
        .context("building routed chat provider for triage")?;
    let slug_hint = resolved
        .find(':')
        .map(|i| &resolved[..i])
        .unwrap_or(resolved.as_str())
        .to_string();
    // `Box<dyn Provider>` → `Arc<dyn Provider>` is a single reallocation
    // — the `Provider` trait is `Send + Sync` so this is type-safe.
    let provider: Arc<dyn Provider> = Arc::from(provider_box);
    tracing::debug!(
        provider = %slug_hint,
        model = %model,
        "[triage::routing] resolved remote provider via workload factory"
    );
    Ok(ResolvedProvider {
        provider,
        provider_name: slug_hint,
        model,
        used_local: false,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "routing_tests.rs"]
mod tests;
