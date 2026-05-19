//! Google OAuth 2.0 + PKCE provider.
//!
//! Wire formats are pinned to the public endpoints documented at
//! <https://developers.google.com/identity/protocols/oauth2/native-app>.
//! In the native-app flow we use the installed-application client type
//! with a `127.0.0.1` loopback redirect (RFC 8252 §7.3) and PKCE S256.

/// Authorization endpoint — user-agent redirected here to consent.
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Token endpoint — server-to-server POST for code exchange and refresh.
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// Default scopes requested for a fresh Google connection. Covers the
/// Gmail + Calendar + Drive surface area that Composio previously
/// brokered, plus `openid`/`email`/`profile` so we can identify the
/// connected account for the UI.
///
/// `drive.file` is the per-file scope (not full Drive) — strictly what
/// the user picks. `gmail.readonly` + `gmail.send` are the read/send
/// pair without the destructive `gmail.modify`/`gmail.compose` scopes.
pub const DEFAULT_SCOPES: &[&str] = &[
    "openid",
    "email",
    "profile",
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.send",
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/calendar.events",
    "https://www.googleapis.com/auth/drive.file",
];

/// Parameters for [`build_auth_url`]. Borrowed slices so callers do not
/// have to allocate when they already have the values to hand.
pub struct AuthUrlParams<'a> {
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scopes: &'a [&'a str],
    pub state: &'a str,
    pub code_challenge: &'a str,
}

/// Build the Google authorization URL with PKCE S256 + offline access.
///
/// Pinned parameter set:
///   * `response_type=code`            — authorization-code flow
///   * `code_challenge_method=S256`    — PKCE
///   * `access_type=offline`           — ask Google for a refresh token
///   * `prompt=consent`                — force the consent screen so the
///     refresh token is always returned (Google omits it on subsequent
///     consents otherwise, and we have no other way to recover it).
///   * `include_granted_scopes=true`   — incremental auth — let later
///     flows widen scope without losing what the user already granted.
pub fn build_auth_url(params: &AuthUrlParams<'_>) -> String {
    let scope = params.scopes.join(" ");
    let pairs: [(&str, &str); 10] = [
        ("client_id", params.client_id),
        ("redirect_uri", params.redirect_uri),
        ("response_type", "code"),
        ("scope", &scope),
        ("state", params.state),
        ("code_challenge", params.code_challenge),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("include_granted_scopes", "true"),
    ];
    // `url::Url::parse_with_params` percent-encodes each value using the
    // application/x-www-form-urlencoded set, which is exactly what Google
    // expects on the query string.
    url::Url::parse_with_params(AUTH_ENDPOINT, &pairs)
        .expect("AUTH_ENDPOINT is a known-good URL")
        .into()
}
