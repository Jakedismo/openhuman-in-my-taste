//! Tests for the PKCE primitives in [`super::pkce`].
//!
//! The interesting checks here are:
//!   * RFC 7636 Appendix B test vector — pinning the SHA-256 + base64url
//!     encoding so we will notice any silent regression of the challenge
//!     derivation.
//!   * Character-set + length conformance for `code_verifier`.
//!   * Entropy sanity — two successive calls must not collide. (Birthday
//!     bound on 256 bits is well above any practical test, so a single
//!     inequality check is sufficient.)

use super::pkce::{
    b64url_no_pad, code_challenge, code_verifier, state, STATE_BYTES, VERIFIER_BYTES,
};

const VERIFIER_LEN_CHARS: usize = 43; // ceil(32 * 8 / 6) = 43 unpadded base64 chars
const VERIFIER_CHARSET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

#[test]
fn b64url_no_pad_matches_rfc4648_test_vectors() {
    // Spot-check a couple of plain-ASCII inputs against the standard
    // RFC 4648 §5 base64url-no-padding outputs. These are not PKCE-specific
    // but they pin the encoding behavior our PKCE helpers depend on.
    assert_eq!(b64url_no_pad(b""), "");
    assert_eq!(b64url_no_pad(b"f"), "Zg");
    assert_eq!(b64url_no_pad(b"fo"), "Zm8");
    assert_eq!(b64url_no_pad(b"foo"), "Zm9v");
    assert_eq!(b64url_no_pad(b"foob"), "Zm9vYg");
    assert_eq!(b64url_no_pad(b"fooba"), "Zm9vYmE");
    assert_eq!(b64url_no_pad(b"foobar"), "Zm9vYmFy");
}

#[test]
fn b64url_no_pad_uses_url_safe_alphabet() {
    // Inputs that would force `+` and `/` in standard base64 must yield
    // `-` and `_` here. Specifically, bytes [0xfb, 0xff, 0xbf] encode to
    // `+/+/` in standard base64 → `-_-_` in URL-safe.
    let encoded = b64url_no_pad(&[0xfb, 0xff, 0xbf]);
    assert!(
        !encoded.contains('+') && !encoded.contains('/') && !encoded.contains('='),
        "encoded={encoded}: must use url-safe alphabet with no padding"
    );
    assert_eq!(encoded, "-_-_");
}

#[test]
fn code_verifier_is_43_chars_from_unreserved_set() {
    let v = code_verifier();
    assert_eq!(
        v.len(),
        VERIFIER_LEN_CHARS,
        "verifier should be {VERIFIER_LEN_CHARS} chars for 32 random bytes"
    );
    for c in v.chars() {
        assert!(
            VERIFIER_CHARSET.contains(c),
            "verifier contains illegal char {c:?}: {v}"
        );
    }
}

#[test]
fn code_verifier_two_calls_differ() {
    // Statistical sanity: two fresh verifiers must not collide. At 256 bits
    // of entropy the probability is ~2^-256 per pair, so a single inequality
    // is a safe gate.
    let a = code_verifier();
    let b = code_verifier();
    assert_ne!(a, b, "two successive code_verifier() calls collided: {a}");
}

#[test]
fn code_challenge_matches_rfc7636_appendix_b() {
    // RFC 7636 Appendix B fixed test vector. If this fails, the SHA-256 or
    // base64url-no-padding step has drifted.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    assert_eq!(code_challenge(verifier), challenge);
}

#[test]
fn code_challenge_is_deterministic() {
    let v = code_verifier();
    assert_eq!(
        code_challenge(&v),
        code_challenge(&v),
        "code_challenge must be a pure function of its input"
    );
}

#[test]
fn state_token_is_url_safe_and_long_enough() {
    let s = state();
    assert!(!s.is_empty(), "state token must not be empty");
    for c in s.chars() {
        assert!(
            VERIFIER_CHARSET.contains(c),
            "state contains non-url-safe char {c:?}: {s}"
        );
    }
    // 16 random bytes encode to 22 unpadded base64 chars. Allow some slack
    // in case the implementation later swaps to a different byte count, but
    // require at least the bits of entropy we declared in STATE_BYTES.
    let min_chars = ((STATE_BYTES * 8) + 5) / 6;
    assert!(
        s.len() >= min_chars,
        "state token shorter than {min_chars} chars (got {len}): {s}",
        len = s.len()
    );
}

#[test]
fn state_two_calls_differ() {
    let a = state();
    let b = state();
    assert_ne!(a, b, "two successive state() calls collided: {a}");
}

#[test]
fn verifier_bytes_constant_matches_43_char_output() {
    // Guard against someone bumping VERIFIER_BYTES without updating the
    // documented 43-char length contract. ceil(N * 8 / 6) chars.
    let expected_chars = (VERIFIER_BYTES * 8 + 5) / 6;
    assert_eq!(
        expected_chars, VERIFIER_LEN_CHARS,
        "VERIFIER_BYTES={VERIFIER_BYTES} encodes to {expected_chars} chars, \
         but the verifier-length test expects {VERIFIER_LEN_CHARS}"
    );
}
