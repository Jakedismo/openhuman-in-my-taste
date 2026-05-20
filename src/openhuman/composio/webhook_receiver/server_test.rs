// ABOUTME: HTTP-level tests for the local Composio webhook receiver.
// ABOUTME: Spins the Axum router on an ephemeral loopback port, drives it with reqwest.

use super::{serve, ReceiverState};

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::openhuman::config::Config;
use crate::openhuman::credentials::ops::store_composio_webhook_secret;

type HmacSha256 = Hmac<Sha256>;

const SECRET: &str = "whsec_test_secret_for_receiver";

fn config_with_secret(tmp: &tempfile::TempDir) -> Arc<Config> {
    let mut c = Config::default();
    c.workspace_dir = tmp.path().join("workspace");
    c.config_path = tmp.path().join("config.toml");
    store_composio_webhook_secret(&c, SECRET).expect("store webhook secret");
    Arc::new(c)
}

fn config_without_secret(tmp: &tempfile::TempDir) -> Arc<Config> {
    let mut c = Config::default();
    c.workspace_dir = tmp.path().join("workspace");
    c.config_path = tmp.path().join("config.toml");
    Arc::new(c)
}

fn sign(id: &str, ts: &str, body: &[u8], secret: &[u8]) -> String {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret).expect("hmac key install");
    mac.update(id.as_bytes());
    mac.update(b".");
    mac.update(ts.as_bytes());
    mac.update(b".");
    mac.update(body);
    let sig = mac.finalize().into_bytes();
    format!("v1,{}", BASE64_STANDARD.encode(sig))
}

async fn spawn_receiver(config: Arc<Config>) -> (String, tokio::task::JoinHandle<()>) {
    // Pick an ephemeral port by binding to 0, then read the port back
    // so we don't race other tests on a fixed port. We can't use the
    // public `serve` directly because it binds to a known port — go
    // through the underlying primitives.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral");
    let port = listener.local_addr().expect("local_addr").port();
    let state = ReceiverState { config };
    let router = super::build_router(state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

fn unix_now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[tokio::test]
async fn healthz_returns_ok_without_auth() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config_without_secret(&tmp);
    let (base, _h) = spawn_receiver(config).await;
    let resp = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(body.contains("composio-webhook-receiver: ok"));
}

#[tokio::test]
async fn webhook_with_no_stored_secret_returns_503() {
    // Before ensure_subscription has run, the receiver has nothing to
    // verify against. Composio holds retries on 503 — exactly the
    // right shape for "transient setup window".
    let tmp = tempfile::tempdir().unwrap();
    let config = config_without_secret(&tmp);
    let (base, _h) = spawn_receiver(config).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook"))
        .header("webhook-id", "msg_1")
        .header("webhook-timestamp", unix_now_secs().to_string())
        .header("webhook-signature", "v1,AAAA")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn webhook_with_bad_signature_returns_401() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config_with_secret(&tmp);
    let (base, _h) = spawn_receiver(config).await;
    let body = r#"{"toolkit":"gmail","trigger":"GMAIL_NEW_GMAIL_MESSAGE","payload":{}}"#;
    let ts = unix_now_secs().to_string();
    let bad_sig = format!("v1,{}", BASE64_STANDARD.encode([0u8; 32]));
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook"))
        .header("webhook-id", "msg_attack")
        .header("webhook-timestamp", &ts)
        .header("webhook-signature", &bad_sig)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_with_missing_headers_returns_400() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config_with_secret(&tmp);
    let (base, _h) = spawn_receiver(config).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook"))
        .header("webhook-id", "msg_x")
        // intentionally omitting webhook-timestamp + webhook-signature
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webhook_with_stale_timestamp_returns_401() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config_with_secret(&tmp);
    let (base, _h) = spawn_receiver(config).await;
    let body = r#"{"toolkit":"gmail","trigger":"GMAIL_NEW_GMAIL_MESSAGE","payload":{}}"#;
    // 1 hour old → well outside the 300s tolerance.
    let stale_ts = (unix_now_secs() - 3600).to_string();
    let sig = sign("msg_replay", &stale_ts, body.as_bytes(), SECRET.as_bytes());
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook"))
        .header("webhook-id", "msg_replay")
        .header("webhook-timestamp", stale_ts)
        .header("webhook-signature", sig)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_with_valid_signature_dispatches_to_bus_and_returns_200() {
    let tmp = tempfile::tempdir().unwrap();
    let config = config_with_secret(&tmp);
    let (base, _h) = spawn_receiver(config.clone()).await;

    // Subscribe to the bus BEFORE posting so we can prove the
    // dispatch happened. The bus singleton may already be initialized
    // by other tests in the same binary — init_global is idempotent
    // (returns the existing instance) so it's safe to call here.
    use crate::core::event_bus::{init_global, DomainEvent};
    let bus = init_global(crate::core::event_bus::DEFAULT_CAPACITY);
    let mut rx_bus = bus.raw_receiver();
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    let bus_handle = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(tokio::time::Duration::from_millis(250), rx_bus.recv()).await
            {
                Ok(Ok(ev)) => {
                    if matches!(
                        ev,
                        DomainEvent::ComposioTriggerReceived {
                            ref toolkit,
                            ref trigger,
                            ..
                        } if toolkit == "gmail" && trigger == "GMAIL_NEW_GMAIL_MESSAGE"
                    ) {
                        let _ = tx.send(true);
                        return;
                    }
                }
                _ => continue,
            }
        }
        let _ = tx.send(false);
    });

    // Composio v3 envelope shape: {id, timestamp, type, metadata, data}
    // where `type = "composio.trigger.message"` and the actual trigger
    // slug lives in `metadata.trigger_slug`. See `WebhookTriggerPayloadV3`
    // in https://github.com/ComposioHQ/composio/blob/next/python/composio/core/models/triggers.py
    let body = serde_json::json!({
        "id": "evt_unit_test",
        "timestamp": "2026-05-20T22:00:00Z",
        "type": "composio.trigger.message",
        "metadata": {
            "log_id": "log_unit_test",
            "trigger_slug": "GMAIL_NEW_GMAIL_MESSAGE",
            "trigger_id": "ti_unit_test",
            "connected_account_id": "ca_unit_test",
            "auth_config_id": "ac_unit_test",
            "user_id": "u_unit_test"
        },
        "data": {"messageId": "12345"}
    });
    let body_str = serde_json::to_string(&body).unwrap();
    let ts = unix_now_secs().to_string();
    let sig = sign("msg_valid", &ts, body_str.as_bytes(), SECRET.as_bytes());
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook"))
        .header("webhook-id", "msg_valid")
        .header("webhook-timestamp", ts)
        .header("webhook-signature", sig)
        .body(body_str)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let dispatched = tokio::time::timeout(tokio::time::Duration::from_secs(6), rx)
        .await
        .expect("dispatch confirmation timed out")
        .expect("oneshot dropped");
    assert!(
        dispatched,
        "verified delivery must have published DomainEvent::ComposioTriggerReceived"
    );
    bus_handle.abort();
}

#[tokio::test]
async fn webhook_with_valid_signature_but_empty_trigger_slug_returns_400() {
    // v3 envelope with type=trigger.message but an empty
    // `metadata.trigger_slug` — the receiver has no way to derive a
    // useful toolkit/trigger pair, so it must reject rather than
    // publish a malformed event onto the bus.
    let tmp = tempfile::tempdir().unwrap();
    let config = config_with_secret(&tmp);
    let (base, _h) = spawn_receiver(config).await;
    let body = serde_json::json!({
        "id": "evt_empty",
        "timestamp": "2026-05-20T22:00:00Z",
        "type": "composio.trigger.message",
        "metadata": {"trigger_slug": ""},
        "data": {}
    });
    let body_str = serde_json::to_string(&body).unwrap();
    let ts = unix_now_secs().to_string();
    let sig = sign(
        "msg_empty_slug",
        &ts,
        body_str.as_bytes(),
        SECRET.as_bytes(),
    );
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook"))
        .header("webhook-id", "msg_empty_slug")
        .header("webhook-timestamp", ts)
        .header("webhook-signature", sig)
        .body(body_str)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webhook_with_connected_account_expired_event_returns_200_without_dispatch() {
    // Currently we only wire trigger events onto the bus. Connection-
    // expired events are acknowledged with 200 (so Composio doesn't
    // retry) but not dispatched — a future commit can add a domain
    // event for them. This test pins that contract so accidentally
    // dispatching the wrong shape onto ComposioTriggerReceived
    // surfaces immediately.
    let tmp = tempfile::tempdir().unwrap();
    let config = config_with_secret(&tmp);
    let (base, _h) = spawn_receiver(config).await;
    let body = serde_json::json!({
        "id": "evt_expired",
        "timestamp": "2026-05-20T22:00:00Z",
        "type": "composio.connected_account.expired",
        "metadata": {},
        "data": {}
    });
    let body_str = serde_json::to_string(&body).unwrap();
    let ts = unix_now_secs().to_string();
    let sig = sign("msg_expired", &ts, body_str.as_bytes(), SECRET.as_bytes());
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhook"))
        .header("webhook-id", "msg_expired")
        .header("webhook-timestamp", ts)
        .header("webhook-signature", sig)
        .body(body_str)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn serve_returns_useful_error_when_port_in_use() {
    // Bind a socket so the next bind on the same port fails.
    let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = blocker.local_addr().unwrap().port();
    let tmp = tempfile::tempdir().unwrap();
    let config = config_without_secret(&tmp);
    let state = ReceiverState { config };
    let err = serve(state, port).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("local_receiver_port"),
        "serve error must point users at the port config knob, got: {msg}"
    );
}
