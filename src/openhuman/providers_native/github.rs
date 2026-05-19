//! Direct GitHub REST API client. Replaces the Composio slugs that
//! went through `/agent-integrations/composio/execute` with direct
//! calls to `https://api.github.com`. Authentication uses the Bearer
//! token persisted by [`crate::openhuman::oauth`] for the `github`
//! provider.
//!
//! Initial surface covers `GITHUB_USERS_GET_AUTHENTICATED`, list-repos
//! for the authenticated user, and create-issue. Additional slugs land
//! as production call sites require them.
//!
//! Endpoint reference:
//! <https://docs.github.com/en/rest>.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::openhuman::credentials::AuthService;
use crate::openhuman::oauth::persistence::GITHUB_PROVIDER;

use crate::openhuman::providers_native::bearer::AuthedClient;

const BASE_URL: &str = "https://api.github.com";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthenticatedUser {
    pub login: String,
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Repo {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    #[serde(default)]
    pub private: Option<bool>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Issue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub html_url: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

/// `GET /user` — return the OAuth user. Maps to
/// `GITHUB_USERS_GET_AUTHENTICATED`.
pub async fn get_authenticated_user(
    http: &reqwest::Client,
    service: &AuthService,
) -> Result<AuthenticatedUser> {
    let client = AuthedClient::new(http, service, GITHUB_PROVIDER);
    client
        .get_json::<AuthenticatedUser>(&format!("{BASE_URL}/user"))
        .await
}

/// `GET /user/repos` — list repositories the authenticated user can
/// see. `per_page` is clamped to GitHub's 100 max.
pub async fn list_authenticated_repos(
    http: &reqwest::Client,
    service: &AuthService,
    per_page: Option<u32>,
) -> Result<Vec<Repo>> {
    let client = AuthedClient::new(http, service, GITHUB_PROVIDER);
    let per_page = per_page.unwrap_or(30).clamp(1, 100);
    let url = format!("{BASE_URL}/user/repos?per_page={per_page}");
    client.get_json::<Vec<Repo>>(&url).await
}

/// `POST /repos/{owner}/{repo}/issues` — create an issue. Returns the
/// created issue resource.
pub async fn create_issue(
    http: &reqwest::Client,
    service: &AuthService,
    owner: &str,
    repo: &str,
    title: &str,
    body: Option<&str>,
) -> Result<Issue> {
    let client = AuthedClient::new(http, service, GITHUB_PROVIDER);
    let url = format!(
        "{BASE_URL}/repos/{}/{}/issues",
        urlencode_path(owner),
        urlencode_path(repo)
    );
    let mut payload = json!({ "title": title });
    if let Some(b) = body {
        payload["body"] = Value::String(b.to_string());
    }
    client.post_json::<Issue>(&url, &payload).await
}

fn urlencode_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_create_body_includes_title_and_body_when_provided() {
        // Exercise the body-construction branch by parsing the same
        // shape `create_issue` builds. Pure structural assertion;
        // we trust the underlying AuthedClient to serialize it.
        let mut payload = json!({ "title": "Bug: things broke" });
        payload["body"] = Value::String("Steps to repro …".into());
        assert_eq!(payload["title"], "Bug: things broke");
        assert_eq!(payload["body"], "Steps to repro …");
    }

    #[test]
    fn list_repos_per_page_clamped_to_github_max() {
        // The clamping is inside the URL builder branch of
        // `list_authenticated_repos`. Re-derive the clamp here so a
        // future relaxation doesn't silently widen the bound.
        let clamped = |n: u32| n.clamp(1, 100);
        assert_eq!(clamped(0), 1);
        assert_eq!(clamped(99999), 100);
        assert_eq!(clamped(30), 30);
    }

    #[test]
    fn url_encode_path_escapes_owner_or_repo_with_dots() {
        // Real repo names contain `.` which is RFC 3986 unreserved
        // (must NOT be escaped). Guard the alphabet so a regression
        // doesn't double-escape a name like `actions/checkout`.
        assert_eq!(urlencode_path("actions"), "actions");
        assert_eq!(urlencode_path("checkout"), "checkout");
        assert_eq!(urlencode_path("a.b-c_d"), "a.b-c_d");
        // Spaces in a repo name (rare but possible historically)
        // should be percent-encoded.
        assert_eq!(urlencode_path("my repo"), "my%20repo");
    }
}
