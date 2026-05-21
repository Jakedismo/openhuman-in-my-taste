//! PKCE (RFC 7636) and CSRF-state primitives for the native OAuth domain.
//!
//! - [`code_verifier`] returns a 43-character `[A-Z][a-z][0-9]-._~` string
//!   suitable for the `code_verifier` parameter (32 random bytes,
//!   base64url-no-padding-encoded).
//! - [`code_challenge`] derives the matching `S256` challenge — `BASE64URL(
//!   SHA256(code_verifier))` without padding.
//! - [`state`] returns an opaque random token for the OAuth `state` parameter
//!   (CSRF defense + flow correlation).
//!
//! All values are URL-safe and need no further escaping when placed in query
//! strings.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

/// Length of the random byte buffer behind a `code_verifier`. 32 bytes
/// produces a 43-character base64url string, which sits inside RFC 7636's
/// `[43, 128]` length window and well above the 256-bit entropy floor most
/// providers expect.
pub const VERIFIER_BYTES: usize = 32;

/// Length of the random byte buffer behind a `state` token. 16 bytes
/// (128 bits) is more than enough to make collisions or guessing
/// computationally infeasible for the lifetime of a single OAuth flow.
pub const STATE_BYTES: usize = 16;

/// Base64url-no-padding-encode `bytes`. Output uses the URL-safe alphabet
/// `[A-Z][a-z][0-9]-_` and contains no `=` padding, matching what every
/// PKCE-aware OAuth server expects on the wire.
pub fn b64url_no_pad(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Generate a fresh `code_verifier` per RFC 7636. The returned string is
/// 43 ASCII characters from the unreserved set and is suitable for direct
/// inclusion in the token-exchange request body.
pub fn code_verifier() -> String {
    let mut buf = [0u8; VERIFIER_BYTES];
    rand::rng().fill_bytes(&mut buf);
    b64url_no_pad(&buf)
}

/// Derive the S256 `code_challenge` for a given `code_verifier`. The result
/// is the base64url-no-padding encoding of SHA-256(verifier).
pub fn code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    b64url_no_pad(&hasher.finalize())
}

/// Generate a fresh opaque `state` token. Used to bind the authorization
/// redirect to the flow that initiated it and to defend against CSRF
/// attacks on the loopback callback.
pub fn state() -> String {
    let mut buf = [0u8; STATE_BYTES];
    rand::rng().fill_bytes(&mut buf);
    b64url_no_pad(&buf)
}
