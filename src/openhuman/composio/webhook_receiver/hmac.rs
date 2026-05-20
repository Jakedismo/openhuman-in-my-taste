// ABOUTME: Svix-style HMAC signature verifier for Composio webhook deliveries.
// ABOUTME: Pure (no I/O) so it stays trivially testable with known vectors.

//! Composio webhook signature verification.
//!
//! Composio signs outbound webhook deliveries using the Svix format
//! ([reference](https://docs.composio.dev/docs/webhook-verification)):
//!
//! - Headers: `webhook-id`, `webhook-timestamp`, `webhook-signature`.
//! - Signed payload: `{webhook_id}.{webhook_timestamp}.{body}` (UTF-8,
//!   period-separated, body verbatim — no whitespace normalisation).
//! - Algorithm: HMAC-SHA256.
//! - Wire format of `webhook-signature` header: a space-separated list
//!   of `v1,<base64-signature>` tokens. A delivery is valid if **any**
//!   of the v1 entries match (Composio rotates secrets by emitting
//!   multiple signatures during the overlap window).
//!
//! This module is I/O-free. The receiver in
//! [`super::server`] is responsible for extracting headers / body /
//! secret and calling [`verify`].
//!
//! Constant-time comparison is essential — a timing-side-channel leak
//! on the secret would let an attacker forge events at will. We use
//! `subtle::ConstantTimeEq` via the `ct_eq` helper on the decoded
//! signature bytes (same byte length always — 32 bytes for SHA-256).

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Errors returned from [`verify`]. Distinct variants so the receiver
/// can pick the right HTTP status (`400` for malformed headers, `401`
/// for signature mismatch, etc.).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("composio webhook: missing required header `{0}`")]
    MissingHeader(&'static str),

    #[error("composio webhook: `webhook-timestamp` is not a valid unix epoch seconds value")]
    InvalidTimestamp,

    #[error(
        "composio webhook: `webhook-timestamp` is outside the {tolerance_secs}s tolerance window \
         (delta={delta_secs}s)"
    )]
    TimestampOutOfWindow {
        delta_secs: i64,
        tolerance_secs: i64,
    },

    #[error("composio webhook: `webhook-signature` header contains no usable v1 entries")]
    NoV1Signatures,

    #[error("composio webhook: signature mismatch (none of the v1 entries verified)")]
    SignatureMismatch,

    #[error("composio webhook: secret could not be installed into the HMAC primitive")]
    InvalidSecret,
}

/// Default tolerance for `webhook-timestamp` drift — Svix's reference
/// recommendation. Five minutes is also what the Composio receiver-side
/// example reaches for; we mirror that so legitimate retries during a
/// brief network blip don't get rejected as replays.
pub const DEFAULT_TIMESTAMP_TOLERANCE_SECS: i64 = 300;

/// Verify a Composio webhook delivery's HMAC signature.
///
/// `body` is the raw request body bytes, exactly as Composio sent them.
/// Do **not** trim, re-encode, or pretty-print before calling — the
/// signature covers the bytes verbatim.
///
/// `now_unix_secs` is the receiver's current unix epoch in seconds.
/// Pulled out as a parameter so tests can pin it; production callers
/// pass `SystemTime::now()` converted to seconds.
///
/// Returns `Ok(())` if at least one `v1,` entry in the
/// `webhook-signature` header verifies and the timestamp is within
/// `tolerance_secs` of `now_unix_secs`. Otherwise returns a typed
/// [`VerifyError`] the caller can map to an HTTP status.
pub fn verify(
    webhook_id: Option<&str>,
    webhook_timestamp: Option<&str>,
    webhook_signature: Option<&str>,
    body: &[u8],
    secret: &[u8],
    now_unix_secs: i64,
    tolerance_secs: i64,
) -> Result<(), VerifyError> {
    let id = webhook_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(VerifyError::MissingHeader("webhook-id"))?;
    let ts_str = webhook_timestamp
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(VerifyError::MissingHeader("webhook-timestamp"))?;
    let signatures = webhook_signature
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(VerifyError::MissingHeader("webhook-signature"))?;

    let ts: i64 = ts_str.parse().map_err(|_| VerifyError::InvalidTimestamp)?;
    let delta = now_unix_secs - ts;
    if delta.abs() > tolerance_secs {
        return Err(VerifyError::TimestampOutOfWindow {
            delta_secs: delta,
            tolerance_secs,
        });
    }

    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(secret).map_err(|_| VerifyError::InvalidSecret)?;
    mac.update(id.as_bytes());
    mac.update(b".");
    mac.update(ts_str.as_bytes());
    mac.update(b".");
    mac.update(body);
    let expected = mac.finalize().into_bytes();

    let mut saw_v1 = false;
    for token in signatures.split_whitespace() {
        let Some(payload) = token.strip_prefix("v1,") else {
            // Composio reserves the `vN,` prefix for future versions.
            // Unknown versions are simply ignored rather than failing
            // the whole delivery — same as Svix's reference behavior.
            continue;
        };
        saw_v1 = true;
        let Ok(decoded) = BASE64_STANDARD.decode(payload) else {
            // Skip individually malformed tokens; another well-formed
            // one in the list might still match.
            continue;
        };
        if decoded.as_slice().ct_eq(expected.as_slice()).into() {
            return Ok(());
        }
    }
    if !saw_v1 {
        return Err(VerifyError::NoV1Signatures);
    }
    Err(VerifyError::SignatureMismatch)
}

#[cfg(test)]
#[path = "hmac_test.rs"]
mod tests;
