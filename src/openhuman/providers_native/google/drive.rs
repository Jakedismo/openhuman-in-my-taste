//! Direct Google Drive REST API client. Limited surface — the
//! production OAuth scope is `drive.file` (per-file access only), so
//! the operations we expose are constrained to files this app
//! created. Endpoint reference:
//! <https://developers.google.com/drive/api/v3/reference>.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::openhuman::credentials::AuthService;
use crate::openhuman::oauth::persistence::GOOGLE_PROVIDER;

use crate::openhuman::providers_native::bearer::AuthedClient;

const BASE_URL: &str = "https://www.googleapis.com/drive/v3";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DriveFile {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub web_view_link: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ListFilesResponse {
    #[serde(default)]
    pub files: Vec<DriveFile>,
    #[serde(default)]
    pub next_page_token: Option<String>,
}

/// List files the app has access to (under `drive.file` scope, that
/// means files created by this app). `query` uses Drive's search
/// syntax (e.g. `name contains 'meeting'`). Optional `page_size` is
/// clamped to Drive's 1000 max.
pub async fn list_files(
    http: &reqwest::Client,
    service: &AuthService,
    query: Option<&str>,
    page_size: Option<u32>,
) -> Result<ListFilesResponse> {
    let client = AuthedClient::new(http, service, GOOGLE_PROVIDER);
    let url = build_list_url(query, page_size);
    client.get_json::<ListFilesResponse>(&url).await
}

/// Create a new file with the given JSON metadata (e.g. `{"name":
/// "Notes.txt", "mimeType": "text/plain"}`). This is the metadata-only
/// flavor — actual content upload is a separate two-step flow we don't
/// need yet. Returns the created file's id + metadata.
pub async fn create_file_metadata(
    http: &reqwest::Client,
    service: &AuthService,
    metadata: &Value,
) -> Result<DriveFile> {
    if !metadata.is_object() {
        return Err(anyhow!(
            "create_file_metadata: metadata must be a JSON object, got {metadata}"
        ));
    }
    let client = AuthedClient::new(http, service, GOOGLE_PROVIDER);
    let url = format!("{BASE_URL}/files");
    client.post_json::<DriveFile>(&url, metadata).await
}

/// Fetch a file's metadata by ID. Useful for follow-up reads after
/// list_files — the list response is intentionally trimmed.
pub async fn get_file_metadata(
    http: &reqwest::Client,
    service: &AuthService,
    file_id: &str,
) -> Result<DriveFile> {
    let client = AuthedClient::new(http, service, GOOGLE_PROVIDER);
    let url = format!(
        "{BASE_URL}/files/{}?fields=id,name,mimeType,webViewLink",
        urlencode(file_id)
    );
    client.get_json::<DriveFile>(&url).await
}

#[allow(dead_code)]
pub(crate) fn json_helper_no_op(_: &Value) {}

pub(crate) fn build_list_url(query: Option<&str>, page_size: Option<u32>) -> String {
    let mut url =
        format!("{BASE_URL}/files?fields=files(id,name,mimeType,webViewLink),nextPageToken");
    if let Some(q) = query {
        url.push_str("&q=");
        url.push_str(&urlencode(q));
    }
    if let Some(n) = page_size {
        let n = n.clamp(1, 1000);
        url.push_str(&format!("&pageSize={n}"));
    }
    url
}

fn urlencode(value: &str) -> String {
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
    use serde_json::json;

    #[test]
    fn list_url_carries_query_and_clamped_page_size() {
        let url = build_list_url(Some("name contains 'plan'"), Some(9999));
        assert!(url.contains("q=name%20contains%20%27plan%27"), "{url}");
        assert!(url.contains("pageSize=1000"), "must clamp to 1000: {url}");
    }

    #[test]
    fn list_url_requests_fields_projection() {
        // Drive returns nothing in `files[]` by default — we MUST
        // request a fields projection or callers see empty entries.
        let url = build_list_url(None, None);
        assert!(
            url.contains("fields=files%28id%2Cname%2CmimeType%2CwebViewLink%29%2CnextPageToken")
                || url.contains("fields=files("),
            "list URL must request a fields projection: {url}"
        );
    }

    #[test]
    fn create_file_metadata_rejects_non_object_input() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let svc = AuthService::new(dir.path(), true);
        let http = reqwest::Client::new();
        let err = rt
            .block_on(create_file_metadata(&http, &svc, &json!("not an object")))
            .unwrap_err();
        assert!(err.to_string().contains("must be a JSON object"));
    }
}
