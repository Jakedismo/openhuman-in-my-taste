use super::*;
use crate::openhuman::config::schema::cloud_providers::{AuthStyle, CloudProviderCreds};
use crate::openhuman::config::Config;
use crate::openhuman::credentials::AuthService;
use tempfile::TempDir;

fn config_with_providers(providers: Vec<CloudProviderCreds>) -> Config {
    let mut c = Config::default();
    c.cloud_providers = providers;
    c
}

fn config_with_providers_in_tempdir(tmp: &TempDir, providers: Vec<CloudProviderCreds>) -> Config {
    let mut c = config_with_providers(providers);
    c.workspace_dir = tmp.path().join("workspace");
    c.config_path = tmp.path().join("config.toml");
    c
}

fn oh_entry(id: &str) -> CloudProviderCreds {
    CloudProviderCreds {
        id: id.to_string(),
        slug: "openhuman".to_string(),
        label: "OpenHuman".to_string(),
        endpoint: "https://api.openhuman.ai/v1".to_string(),
        auth_style: AuthStyle::OpenhumanJwt,
        ..Default::default()
    }
}

fn openai_entry(id: &str, slug: &str) -> CloudProviderCreds {
    CloudProviderCreds {
        id: id.to_string(),
        slug: slug.to_string(),
        label: "OpenAI".to_string(),
        endpoint: "https://api.openai.com/v1".to_string(),
        auth_style: AuthStyle::Bearer,
        default_model: Some("gpt-4o".to_string()),
        ..Default::default()
    }
}

fn anthropic_entry(id: &str, slug: &str) -> CloudProviderCreds {
    CloudProviderCreds {
        id: id.to_string(),
        slug: slug.to_string(),
        label: "Anthropic".to_string(),
        endpoint: "https://api.anthropic.com/v1".to_string(),
        auth_style: AuthStyle::Anthropic,
        default_model: Some("claude-sonnet-4-6".to_string()),
        ..Default::default()
    }
}

#[test]
fn openhuman_literal_errors_in_local_fork() {
    // In the local-OAuth fork the OpenHuman backend is gone; the
    // sentinel must hard-error with a pointer at Settings → AI so
    // the user knows where to configure their own provider.
    let config = Config::default();
    let err = create_chat_provider_from_string("reasoning", "openhuman", &config)
        .err()
        .expect("must error");
    assert!(
        err.to_string()
            .contains("OpenHuman backend provider is not available"),
        "expected backend-removed error, got: {err}"
    );
}

#[test]
fn cloud_no_providers_errors_in_local_fork() {
    // With no cloud_providers configured, "cloud" sentinel falls
    // through to the same OpenHuman-backend hard error.
    let config = Config::default();
    let err = create_chat_provider_from_string("reasoning", "cloud", &config)
        .err()
        .expect("must error");
    assert!(
        err.to_string()
            .contains("OpenHuman backend provider is not available"),
        "expected backend-removed error, got: {err}"
    );
}

#[test]
fn openhuman_slug_errors_in_local_fork() {
    // `openhuman:<model>` used to route to the backend provider.
    // After the local-OAuth refactor it must hard-error too.
    let config = config_with_providers(vec![oh_entry("p_oh")]);
    let err = create_chat_provider_from_string("reasoning", "openhuman:", &config)
        .err()
        .expect("must error");
    assert!(
        err.to_string()
            .contains("OpenHuman backend provider is not available"),
        "expected backend-removed error, got: {err}"
    );
}

#[test]
fn openai_slug_model() {
    let config = config_with_providers(vec![openai_entry("p_oai", "openai")]);
    let (_, model) = create_chat_provider_from_string("agentic", "openai:gpt-4o-mini", &config)
        .expect("openai:<model> must build");
    assert_eq!(model, "gpt-4o-mini");
}

#[test]
fn anthropic_slug_model() {
    let config = config_with_providers(vec![anthropic_entry("p_ant", "anthropic")]);
    let (_, model) =
        create_chat_provider_from_string("coding", "anthropic:claude-sonnet-4-6", &config)
            .expect("anthropic:<model> must build");
    assert_eq!(model, "claude-sonnet-4-6");
}

#[test]
fn openrouter_slug_model() {
    let mut config = Config::default();
    config.cloud_providers.push(CloudProviderCreds {
        id: "p_or".to_string(),
        slug: "openrouter".to_string(),
        label: "OpenRouter".to_string(),
        endpoint: "https://openrouter.ai/api/v1".to_string(),
        auth_style: AuthStyle::Bearer,
        default_model: Some("openai/gpt-4o".to_string()),
        ..Default::default()
    });
    let (_, model) =
        create_chat_provider_from_string("agentic", "openrouter:meta-llama/llama-3.1-8b", &config)
            .expect("openrouter:<model> must build");
    assert_eq!(model, "meta-llama/llama-3.1-8b");
}

#[test]
fn ollama_prefix() {
    let config = Config::default();
    let (_, model) = create_chat_provider_from_string("heartbeat", "ollama:llama3.1:8b", &config)
        .expect("ollama:<model> must build");
    assert_eq!(model, "llama3.1:8b");
}

#[test]
fn cloud_provider_with_empty_model_falls_back_to_row_default() {
    // The triage path resolves `provider_for_role("chat", ...)` → if
    // the user only has an anthropic cloud_providers row with a
    // `default_model` set, the resolved string is `anthropic:` (no
    // model). The factory must hydrate the empty model from the row's
    // `default_model` instead of forwarding `model: ""` and letting
    // the upstream reject with `model: String should have at least 1
    // character`.
    let mut config = Config::default();
    config.cloud_providers.push(CloudProviderCreds {
        id: "p_ant".into(),
        slug: "anthropic".into(),
        label: "Anthropic".into(),
        endpoint: "https://api.anthropic.com/v1".into(),
        auth_style: AuthStyle::Anthropic,
        default_model: Some("claude-sonnet-4-6".into()),
        ..Default::default()
    });
    let (_, model) = create_chat_provider_from_string("chat", "anthropic:", &config)
        .expect("empty model should hydrate from row's default_model");
    assert_eq!(model, "claude-sonnet-4-6");
}

#[test]
fn cloud_provider_with_empty_model_and_no_defaults_errors_actionably() {
    // No model in the resolver string, no row `default_model`, no
    // matching global `default_model`. Factory must surface a clear
    // "no model configured" error pointing the user at the right
    // config knob — sending `model: ""` to Anthropic would otherwise
    // be silent until the API rejected it.
    let mut config = Config::default();
    config.default_model = None;
    config.cloud_providers.push(CloudProviderCreds {
        id: "p_ant".into(),
        slug: "anthropic".into(),
        label: "Anthropic".into(),
        endpoint: "https://api.anthropic.com/v1".into(),
        auth_style: AuthStyle::Anthropic,
        default_model: None,
        ..Default::default()
    });
    let err = create_chat_provider_from_string("chat", "anthropic:", &config)
        .err()
        .expect("must error when no model is configured");
    let msg = err.to_string();
    assert!(
        msg.contains("no model configured for slug 'anthropic'"),
        "expected actionable no-model error, got: {msg}"
    );
}

#[test]
fn provider_for_role_chat_uses_chat_provider() {
    // Regression for the GitHub trigger triage path: `role="chat"`
    // was missing from `provider_for_role`'s match, so the resolver
    // fell through to `primary_cloud` synthesis (`<slug>:` with no
    // model). With the new `"chat"` arm, an explicit `chat_provider`
    // is honoured the same as `reasoning_provider` etc.
    let mut config = Config::default();
    config.cloud_providers.push(openai_entry("p_oai", "openai"));
    config.chat_provider = Some("openai:gpt-4o-mini".to_string());
    assert_eq!(provider_for_role("chat", &config), "openai:gpt-4o-mini");
}

#[test]
fn provider_for_role_chat_falls_back_to_default_model() {
    // Without `chat_provider` set, `role="chat"` inherits the
    // global `default_model` (the user's chosen LLM). This is the
    // path the triage agent took when no override existed for chat —
    // pre-fix it returned `<slug>:` with no model and the upstream
    // rejected with `model: String should have at least 1 character`.
    let mut config = Config::default();
    config.cloud_providers.push(openai_entry("p_oai", "openai"));
    config.default_model = Some("openai:gpt-5.4".to_string());
    assert_eq!(provider_for_role("chat", &config), "openai:gpt-5.4");
}

#[test]
fn local_runtime_slug_with_auth_none_skips_auth_lookup() {
    // Regression for the LM Studio failure where the chat-factory's
    // Bearer arm hit the auth store on every provider build and
    // surfaced `failed to read API key for slug 'lmstudio': Timed out
    // waiting for auth profile lock`. With `auth_style = None` the
    // factory must build the provider WITHOUT calling the auth store.
    let mut config = Config::default();
    config.cloud_providers.push(CloudProviderCreds {
        id: "p_lm".to_string(),
        slug: "lmstudio".to_string(),
        label: "LM Studio".to_string(),
        endpoint: "http://localhost:1234".to_string(),
        auth_style: AuthStyle::None,
        default_model: Some("google/gemma-4-31b".to_string()),
        ..Default::default()
    });
    let (_, model) =
        create_chat_provider_from_string("memory", "lmstudio:google/gemma-4-31b", &config)
            .expect("lmstudio AuthStyle::None build must succeed without auth-store access");
    assert_eq!(model, "google/gemma-4-31b");
}

#[tokio::test]
async fn ollama_provider_does_not_require_api_key() {
    let mut config = Config::default();
    config.local_ai.base_url = Some("http://127.0.0.1:9".to_string());
    let (provider, model) =
        create_chat_provider_from_string("heartbeat", "ollama:llama3.1:8b", &config)
            .expect("ollama:<model> must build");

    let err = provider
        .chat_with_system(None, "hello", &model, 0.0)
        .await
        .expect_err("unreachable local Ollama should still attempt a transport call");
    let msg = err.to_string();
    assert!(
        !msg.contains("API key not set"),
        "ollama path must not fail on missing key: {msg}"
    );
}

#[test]
fn all_workloads_default_to_openhuman_when_default_model_cleared() {
    // With `default_model = None` (and no `cloud_providers` / no
    // `primary_cloud` / no `*_provider`), every workload falls
    // through to the PROVIDER_OPENHUMAN sentinel — which the
    // downstream factory then turns into the actionable
    // "no cloud provider configured" error.
    let mut config = Config::default();
    config.default_model = None;
    for role in &[
        "chat",
        "reasoning",
        "agentic",
        "coding",
        "memory",
        "embeddings",
        "heartbeat",
        "learning",
        "subconscious",
    ] {
        assert_eq!(
            provider_for_role(role, &config),
            "openhuman",
            "role={role} must default to openhuman when nothing else is configured"
        );
    }
}

#[test]
fn workloads_inherit_global_default_model_when_no_role_override() {
    // When `default_model` is set to a `<slug>:<model>` string, every
    // workload that doesn't have its own `*_provider` inherits it.
    // This was the load-bearing fix for the GitHub trigger triage path:
    // role="chat" had no `chat_provider`, so the resolver fell through
    // to `primary_cloud` and synthesised `<slug>:` with no model.
    let mut config = Config::default();
    config.default_model = Some("openai:gpt-5.4".to_string());
    for role in &[
        "chat",
        "reasoning",
        "agentic",
        "coding",
        "memory",
        "embeddings",
        "heartbeat",
        "learning",
        "subconscious",
    ] {
        assert_eq!(
            provider_for_role(role, &config),
            "openai:gpt-5.4",
            "role={role} must inherit default_model when no role override is set"
        );
    }
}

#[test]
fn workload_override_respected() {
    let mut config = Config::default();
    config.default_model = None;
    config.heartbeat_provider = Some("ollama:llama3.2:3b".to_string());
    assert_eq!(
        provider_for_role("heartbeat", &config),
        "ollama:llama3.2:3b"
    );
    assert_eq!(provider_for_role("reasoning", &config), "openhuman");
}

#[test]
fn create_chat_provider_uses_role() {
    let mut config = Config::default();
    config.cloud_providers.push(openai_entry("p_oai", "openai"));
    config.reasoning_provider = Some("openai:gpt-4o-mini".to_string());
    let (_, model) =
        create_chat_provider("reasoning", &config).expect("create_chat_provider must succeed");
    assert_eq!(model, "gpt-4o-mini");
}

#[test]
fn unknown_slug_rejected() {
    let config = Config::default();
    let err = create_chat_provider_from_string("reasoning", "groq:llama3", &config)
        .err()
        .expect("unknown slug must fail");
    assert!(
        err.to_string()
            .contains("no cloud provider configured for slug"),
        "{err}"
    );
}

#[test]
fn bare_string_without_colon_rejected() {
    let config = Config::default();
    let err = create_chat_provider_from_string("reasoning", "openai", &config)
        .err()
        .expect("bare string must fail");
    assert!(
        err.to_string().contains("unrecognised provider string"),
        "{err}"
    );
}

#[test]
fn empty_model_in_ollama_rejected() {
    let config = Config::default();
    let err = create_chat_provider_from_string("reasoning", "ollama:", &config)
        .err()
        .expect("empty model must fail");
    assert!(err.to_string().contains("empty model"), "{err}");
}

#[test]
fn missing_slug_for_openai_gives_clear_error() {
    let config = Config::default();
    let err = create_chat_provider_from_string("reasoning", "openai:gpt-4o", &config)
        .err()
        .expect("missing slug must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("no cloud provider configured for slug 'openai'"),
        "{msg}"
    );
}

#[tokio::test]
async fn cloud_provider_without_stored_key_fails_with_actionable_error() {
    let tmp = TempDir::new().expect("tempdir");
    let config = config_with_providers_in_tempdir(&tmp, vec![openai_entry("p_oai", "openai")]);
    let (provider, model) = create_chat_provider_from_string("reasoning", "openai:gpt-4o", &config)
        .expect("provider should build without eagerly requiring credentials");

    let err = provider
        .chat_with_system(None, "hello", &model, 0.0)
        .await
        .expect_err("missing key should fail at call time");
    assert!(
        err.to_string().contains("cloud API key not set"),
        "expected missing-key guidance, got: {err}"
    );
}

#[tokio::test]
async fn cloud_provider_with_auth_none_does_not_require_api_key() {
    let tmp = TempDir::new().expect("tempdir");
    let mut entry = openai_entry("p_proxy", "proxy");
    entry.auth_style = AuthStyle::None;
    entry.endpoint = "http://127.0.0.1:9".to_string();
    let config = config_with_providers_in_tempdir(&tmp, vec![entry]);
    let (provider, model) = create_chat_provider_from_string("reasoning", "proxy:gpt-oss", &config)
        .expect("auth:none provider must build");

    let err = provider
        .chat_with_system(None, "hello", &model, 0.0)
        .await
        .expect_err("unreachable auth:none endpoint should attempt transport");
    let msg = err.to_string();
    assert!(
        !msg.contains("API key not set"),
        "auth:none provider must not fail on missing key: {msg}"
    );
}

#[tokio::test]
async fn cloud_provider_with_malformed_endpoint_surfaces_url_error() {
    let tmp = TempDir::new().expect("tempdir");
    let mut entry = openai_entry("p_bad", "openai");
    entry.endpoint = "://not a url".to_string();
    let config = config_with_providers_in_tempdir(&tmp, vec![entry]);
    let auth = AuthService::from_config(&config);
    auth.store_provider_token(
        "provider:openai",
        "default",
        "sk-test",
        Default::default(),
        true,
    )
    .expect("store provider token");

    let (provider, model) = create_chat_provider_from_string("reasoning", "openai:gpt-4o", &config)
        .expect("provider should still build");

    let err = provider
        .chat_with_system(None, "hello", &model, 0.0)
        .await
        .expect_err("malformed endpoint should fail at request build/send time");
    let msg = err.to_string().to_ascii_lowercase();
    assert!(
        msg.contains("builder error")
            || msg.contains("relative url without a base")
            || msg.contains("empty host")
            || msg.contains("invalid port"),
        "expected malformed-url style error, got: {msg}"
    );
}

#[test]
fn primary_cloud_with_no_providers_errors_in_local_fork() {
    // No cloud_providers, no primary_cloud, no workload override,
    // and a `default_model = "openai:gpt-5.4"` that points at a slug
    // that isn't in `cloud_providers`. Factory must surface a clear
    // "no cloud provider configured" error pointing the user at
    // Settings → AI, not silently degrade to the dead OpenHuman
    // backend.
    let config = Config::default();
    let err = create_chat_provider("reasoning", &config)
        .err()
        .expect("must error");
    let msg = err.to_string();
    assert!(
        msg.contains("no cloud provider configured")
            || msg.contains("OpenHuman backend provider is not available"),
        "expected a clear 'configure a provider' error, got: {msg}"
    );
}

#[test]
fn summarization_aliases_memory_provider() {
    let mut config = Config::default();
    config.memory_provider = Some("ollama:llama3.1:8b".to_string());
    assert_eq!(provider_for_role("memory", &config), "ollama:llama3.1:8b");
    assert_eq!(
        provider_for_role("summarization", &config),
        "ollama:llama3.1:8b",
        "summarization must alias memory_provider"
    );
}

#[test]
fn summarization_defaults_to_openhuman_like_memory() {
    let mut config = Config::default();
    config.default_model = None;
    assert_eq!(provider_for_role("memory", &config), "openhuman");
    assert_eq!(provider_for_role("summarization", &config), "openhuman");
}

#[test]
fn unknown_workload_falls_back_to_openhuman() {
    let mut config = Config::default();
    config.default_model = None;
    assert_eq!(
        provider_for_role("nope-not-a-workload", &config),
        "openhuman"
    );
    assert_eq!(provider_for_role("", &config), "openhuman");
}

// The `openhuman_backend_uses_config_path_parent_as_state_dir` test
// was removed in the local-OAuth refactor — the OpenHuman backend
// provider is no longer reachable, so there is no state_dir threading
// behaviour left to assert. The user-facing fix when chat hits the
// "no provider" path is to configure one in Settings → AI; this is
// covered by `primary_cloud_with_no_providers_errors_in_local_fork`.

// ── verify_session_active tests ──────────────────────────────────────

/// Helper: build a Config whose `config_path` lives inside a tempdir.
fn config_in_tempdir(tmp: &TempDir) -> Config {
    let mut c = Config::default();
    c.config_path = tmp.path().join("config.toml");
    c
}

// The `verify_session_active` gate (and its four tests) was removed
// in the local-OAuth refactor — the OpenHuman backend session is gone,
// so the "no session → reject custom providers" guard no longer
// applies to a single-user local desktop.
