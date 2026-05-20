// ABOUTME: Contract-input and pure-helper tests for the webhook subscription module.
// ABOUTME: Full three-branch HTTP coverage requires base-URL plumbing on ComposioTool
// ABOUTME: (not yet exposed) — manual end-to-end verification covers the integration
// ABOUTME: case for v1; follow-up can add real round-trip tests once that hook lands.

use super::{dedupe_preserving_order, ensure_subscription, extract_event_set};

use std::sync::Arc;

use crate::openhuman::config::Config;
use crate::openhuman::security::SecurityPolicy;
use crate::openhuman::tools::ComposioTool;

fn test_config(tmp: &tempfile::TempDir) -> Config {
    let mut c = Config::default();
    c.workspace_dir = tmp.path().join("workspace");
    c.config_path = tmp.path().join("config.toml");
    c
}

fn dummy_direct() -> Arc<ComposioTool> {
    Arc::new(ComposioTool::new(
        "ck_test_dummy_key",
        Some("test-entity"),
        Arc::new(SecurityPolicy::default()),
    ))
}

#[tokio::test]
async fn ensure_subscription_rejects_empty_url() {
    let tmp = tempfile::tempdir().unwrap();
    let config = test_config(&tmp);
    let direct = dummy_direct();
    let err = ensure_subscription(&config, &direct, "  ", &["gmail".to_string()], "")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("webhook_url must not be empty"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn ensure_subscription_rejects_non_https_url() {
    let tmp = tempfile::tempdir().unwrap();
    let config = test_config(&tmp);
    let direct = dummy_direct();
    let err = ensure_subscription(
        &config,
        &direct,
        "http://insecure.example.com/webhook",
        &["gmail".to_string()],
        "",
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("must be HTTPS"),
        "non-HTTPS URL must be rejected before any Composio call, got: {err}"
    );
}

#[tokio::test]
async fn ensure_subscription_rejects_empty_event_list() {
    let tmp = tempfile::tempdir().unwrap();
    let config = test_config(&tmp);
    let direct = dummy_direct();
    let err = ensure_subscription(
        &config,
        &direct,
        "https://abc-123.ngrok-free.dev/webhook",
        &[],
        "",
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("at least one event type must be requested"),
        "empty event list must be rejected before any Composio call, got: {err}"
    );
}

#[test]
fn extract_event_set_handles_snake_and_camel_case() {
    let snake = serde_json::json!({
        "enabled_events": ["GMAIL_NEW_GMAIL_MESSAGE", "SLACK_NEW_MESSAGE"]
    });
    assert_eq!(
        extract_event_set(&snake),
        vec![
            "GMAIL_NEW_GMAIL_MESSAGE".to_string(),
            "SLACK_NEW_MESSAGE".to_string(),
        ]
    );

    let camel = serde_json::json!({
        "enabledEvents": ["GITHUB_PUSH"]
    });
    assert_eq!(extract_event_set(&camel), vec!["GITHUB_PUSH".to_string()]);

    let neither = serde_json::json!({"foo": "bar"});
    assert!(extract_event_set(&neither).is_empty());
}

#[test]
fn extract_event_set_drops_empty_and_non_string_entries() {
    let mixed = serde_json::json!({
        "enabled_events": ["GMAIL_NEW_GMAIL_MESSAGE", "", "  ", 42, null, "SLACK_NEW_MESSAGE"]
    });
    assert_eq!(
        extract_event_set(&mixed),
        vec![
            "GMAIL_NEW_GMAIL_MESSAGE".to_string(),
            "SLACK_NEW_MESSAGE".to_string(),
        ]
    );
}

#[test]
fn dedupe_preserving_order_keeps_first_occurrence() {
    let items = vec![
        "GMAIL_NEW_GMAIL_MESSAGE".to_string(),
        "SLACK_NEW_MESSAGE".to_string(),
        "GMAIL_NEW_GMAIL_MESSAGE".to_string(),
        "GITHUB_PUSH".to_string(),
        "SLACK_NEW_MESSAGE".to_string(),
    ];
    assert_eq!(
        dedupe_preserving_order(items),
        vec![
            "GMAIL_NEW_GMAIL_MESSAGE".to_string(),
            "SLACK_NEW_MESSAGE".to_string(),
            "GITHUB_PUSH".to_string(),
        ]
    );
}

#[test]
fn dedupe_preserving_order_passes_through_empty() {
    let empty: Vec<String> = vec![];
    assert!(dedupe_preserving_order(empty).is_empty());
}
