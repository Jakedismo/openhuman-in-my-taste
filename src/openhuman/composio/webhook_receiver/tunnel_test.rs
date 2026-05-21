// ABOUTME: Lightweight tunnel state tests — no live ngrok network calls.
// ABOUTME: Real ngrok session tests require a live authtoken + network — those
// ABOUTME: are reserved for manual end-to-end verification.

use super::TunnelState;

#[test]
fn public_url_is_only_some_in_ready_state() {
    assert_eq!(TunnelState::Idle.public_url(), None);
    assert_eq!(TunnelState::Connecting.public_url(), None);
    assert_eq!(
        TunnelState::Error("boom".into()).public_url(),
        None,
        "errored state must not expose a public URL"
    );
    let ready = TunnelState::Ready {
        public_url: "https://abc-123.ngrok-free.dev".into(),
    };
    assert_eq!(ready.public_url(), Some("https://abc-123.ngrok-free.dev"));
}
