use super::*;
use crate::openhuman::config::schema::cloud_providers::{AuthStyle, CloudProviderCreds};

/// Returns a `Config` that has a usable `chat` workload — the local-OAuth
/// fork's triage path now routes through `provider_for_role("chat", …)`,
/// so a bare `Config::default()` (zero `cloud_providers` rows) would
/// surface the "no chat provider configured" error rather than landing on
/// the dead OpenHuman backend like the legacy tests assumed.
fn test_config() -> Config {
    let mut config = Config::default();
    config.cloud_providers.push(CloudProviderCreds {
        id: "p1".into(),
        slug: "openai".into(),
        label: "OpenAI".into(),
        endpoint: "https://api.openai.com/v1".into(),
        auth_style: AuthStyle::Bearer,
        default_model: Some("gpt-5.4".into()),
        ..CloudProviderCreds::default()
    });
    config.primary_cloud = Some("p1".into());
    config
}

#[test]
fn build_remote_provider_uses_workload_factory_and_default_model() {
    let config = test_config();
    let resolved = build_remote_provider(&config).expect("remote provider should build");
    // Factory-routed slug hint should be `openai` (the slug from the
    // primary cloud_providers row), not the legacy backend id.
    assert_eq!(resolved.provider_name, "openai");
    // Model resolves through the factory's `chat` role.
    assert!(
        !resolved.model.is_empty(),
        "factory should hand back a non-empty model"
    );
    assert!(!resolved.used_local, "used_local is always false");
}

#[test]
fn build_remote_provider_errors_when_no_cloud_provider_configured() {
    // Local-OAuth fork: `Config::default()` has no `cloud_providers`
    // and no `chat_provider`, so the factory falls through to the
    // dead OpenHuman backend slug — the resolver pre-empts that with
    // a clear actionable error that matches the rest of the fork's
    // "Settings → AI" pointers.
    let config = Config::default();
    let err = build_remote_provider(&config)
        .err()
        .expect("expected error when no provider is configured");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("no chat provider configured") || msg.contains("Settings → AI"),
        "unexpected error message: {msg}"
    );
}

#[tokio::test]
async fn resolve_provider_with_config_always_returns_remote() {
    // Even when runtime_enabled is true, triage must always use remote.
    let mut config = test_config();
    config.local_ai.runtime_enabled = true;
    let resolved = resolve_provider_with_config(&config)
        .await
        .expect("resolve should succeed");
    assert!(!resolved.used_local, "triage must never use local AI");
    assert_eq!(resolved.provider_name, "openai");
}

#[tokio::test]
async fn resolve_provider_with_config_returns_remote_when_local_disabled() {
    let mut config = test_config();
    config.local_ai.runtime_enabled = false;
    let resolved = resolve_provider_with_config(&config)
        .await
        .expect("resolve should succeed");
    assert!(!resolved.used_local);
    assert_eq!(resolved.provider_name, "openai");
}
