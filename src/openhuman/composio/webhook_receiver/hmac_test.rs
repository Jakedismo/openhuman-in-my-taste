// ABOUTME: Known-vector unit tests for Svix-style Composio webhook HMAC verification.
// ABOUTME: All vectors are deterministic — no clock, no network, no entropy.

use super::{verify, VerifyError, DEFAULT_TIMESTAMP_TOLERANCE_SECS};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Build a valid signature for a given (id, ts, body, secret) tuple.
/// Used to seed positive-path test vectors so we know we're comparing
/// against the right HMAC output.
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

const SECRET: &[u8] = b"whsec_test_secret_for_unit_tests";
const ID: &str = "msg_abc_123";
const TS: &str = "1747787612"; // 2026-05-20T22:46:52Z — within window if now=TS
const BODY: &str = r#"{"toolkit":"gmail","trigger":"GMAIL_NEW_GMAIL_MESSAGE"}"#;

#[test]
fn verify_accepts_well_formed_signature_in_window() {
    let sig = sign(ID, TS, BODY.as_bytes(), SECRET);
    let now: i64 = TS.parse().unwrap();
    assert_eq!(
        verify(
            Some(ID),
            Some(TS),
            Some(&sig),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS,
        ),
        Ok(())
    );
}

#[test]
fn verify_accepts_when_one_of_several_v1_entries_matches() {
    // Svix-style header may carry multiple `v1,` tokens during a secret
    // rotation overlap — Composio rotates by emitting both old and new
    // signatures for a window. Receiver must accept the delivery when
    // ANY of them validates.
    let good = sign(ID, TS, BODY.as_bytes(), SECRET);
    let bogus = "v1,AAAABBBBCCCC=";
    let combined = format!("{bogus} {good}");
    let now: i64 = TS.parse().unwrap();
    assert_eq!(
        verify(
            Some(ID),
            Some(TS),
            Some(&combined),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS,
        ),
        Ok(())
    );
}

#[test]
fn verify_skips_unknown_version_prefixes() {
    // Future versions (v2, v3 …) appear in the header alongside v1.
    // Receiver must ignore them rather than fail. Provide ONLY a future
    // version → counts as "no usable v1 entries".
    let now: i64 = TS.parse().unwrap();
    let err = verify(
        Some(ID),
        Some(TS),
        Some("v2,somefutureformatpayload="),
        BODY.as_bytes(),
        SECRET,
        now,
        DEFAULT_TIMESTAMP_TOLERANCE_SECS,
    )
    .unwrap_err();
    assert_eq!(err, VerifyError::NoV1Signatures);
}

#[test]
fn verify_rejects_when_signature_does_not_match() {
    // Right shape, wrong bytes — must surface SignatureMismatch (not
    // a malformed-input error) so the caller can log "active attack
    // attempt" semantics.
    let bad_sig = format!("v1,{}", BASE64_STANDARD.encode([0u8; 32]));
    let now: i64 = TS.parse().unwrap();
    assert_eq!(
        verify(
            Some(ID),
            Some(TS),
            Some(&bad_sig),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS,
        ),
        Err(VerifyError::SignatureMismatch)
    );
}

#[test]
fn verify_rejects_when_body_is_tampered() {
    // Same headers + secret, but the body has been mutated in transit
    // (added a trailing space). HMAC binds the body byte-for-byte; the
    // signature must no longer validate.
    let sig = sign(ID, TS, BODY.as_bytes(), SECRET);
    let tampered = format!("{BODY} ");
    let now: i64 = TS.parse().unwrap();
    assert_eq!(
        verify(
            Some(ID),
            Some(TS),
            Some(&sig),
            tampered.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS,
        ),
        Err(VerifyError::SignatureMismatch)
    );
}

#[test]
fn verify_rejects_outdated_timestamp() {
    // Replay attempt: signature is internally consistent, but the
    // timestamp is 10 minutes old (300s tolerance). Must reject with
    // TimestampOutOfWindow so the receiver can return 401 instead of
    // dispatching a stale event.
    let sig = sign(ID, TS, BODY.as_bytes(), SECRET);
    let now: i64 = TS.parse::<i64>().unwrap() + 600;
    let err = verify(
        Some(ID),
        Some(TS),
        Some(&sig),
        BODY.as_bytes(),
        SECRET,
        now,
        DEFAULT_TIMESTAMP_TOLERANCE_SECS,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        VerifyError::TimestampOutOfWindow {
            tolerance_secs: 300,
            ..
        }
    ));
}

#[test]
fn verify_rejects_future_timestamp_outside_window() {
    // Symmetric to the past case — a clock-skewed sender could push
    // a timestamp 10 minutes ahead. Same out-of-window rejection.
    let sig = sign(ID, TS, BODY.as_bytes(), SECRET);
    let now: i64 = TS.parse::<i64>().unwrap() - 600;
    let err = verify(
        Some(ID),
        Some(TS),
        Some(&sig),
        BODY.as_bytes(),
        SECRET,
        now,
        DEFAULT_TIMESTAMP_TOLERANCE_SECS,
    )
    .unwrap_err();
    assert!(matches!(err, VerifyError::TimestampOutOfWindow { .. }));
}

#[test]
fn verify_rejects_non_numeric_timestamp() {
    let sig = sign(ID, TS, BODY.as_bytes(), SECRET);
    let now: i64 = TS.parse().unwrap();
    assert_eq!(
        verify(
            Some(ID),
            Some("not-a-timestamp"),
            Some(&sig),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS,
        ),
        Err(VerifyError::InvalidTimestamp)
    );
}

#[test]
fn verify_rejects_missing_headers() {
    let sig = sign(ID, TS, BODY.as_bytes(), SECRET);
    let now: i64 = TS.parse().unwrap();
    assert_eq!(
        verify(
            None,
            Some(TS),
            Some(&sig),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS
        ),
        Err(VerifyError::MissingHeader("webhook-id"))
    );
    assert_eq!(
        verify(
            Some(ID),
            None,
            Some(&sig),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS
        ),
        Err(VerifyError::MissingHeader("webhook-timestamp"))
    );
    assert_eq!(
        verify(
            Some(ID),
            Some(TS),
            None,
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS
        ),
        Err(VerifyError::MissingHeader("webhook-signature"))
    );
    // Whitespace-only counts as missing, not as a malformed value.
    assert_eq!(
        verify(
            Some("   "),
            Some(TS),
            Some(&sig),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS
        ),
        Err(VerifyError::MissingHeader("webhook-id"))
    );
}

#[test]
fn verify_rejects_when_secret_is_different() {
    // Sender used secret A; receiver compares with secret B (e.g. user
    // forgot to update after rotating). Must reject.
    let sig = sign(ID, TS, BODY.as_bytes(), b"other-secret");
    let now: i64 = TS.parse().unwrap();
    assert_eq!(
        verify(
            Some(ID),
            Some(TS),
            Some(&sig),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS,
        ),
        Err(VerifyError::SignatureMismatch)
    );
}

#[test]
fn verify_accepts_tolerance_window_edge() {
    // Exactly at the tolerance boundary (now == ts + tolerance). The
    // check is `delta.abs() > tolerance`, so equal must accept.
    let sig = sign(ID, TS, BODY.as_bytes(), SECRET);
    let now: i64 = TS.parse::<i64>().unwrap() + DEFAULT_TIMESTAMP_TOLERANCE_SECS;
    assert_eq!(
        verify(
            Some(ID),
            Some(TS),
            Some(&sig),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS,
        ),
        Ok(())
    );
}

#[test]
fn verify_skips_malformed_base64_individually() {
    // One token has invalid base64 padding, the other is valid. The
    // malformed one must not poison the verification.
    let good = sign(ID, TS, BODY.as_bytes(), SECRET);
    let combined = format!("v1,!!!notbase64!!! {good}");
    let now: i64 = TS.parse().unwrap();
    assert_eq!(
        verify(
            Some(ID),
            Some(TS),
            Some(&combined),
            BODY.as_bytes(),
            SECRET,
            now,
            DEFAULT_TIMESTAMP_TOLERANCE_SECS,
        ),
        Ok(())
    );
}
