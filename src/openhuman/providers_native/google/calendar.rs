//! Direct Google Calendar REST API client. Replaces the Composio
//! slugs `GOOGLECALENDAR_EVENTS_LIST`, `GOOGLECALENDAR_FIND_EVENT`,
//! `GOOGLECALENDAR_EVENTS_GET`, `GOOGLECALENDAR_CREATE_EVENT`.
//!
//! Endpoint reference:
//! <https://developers.google.com/calendar/api/v3/reference>.
//!
//! Defaulting behavior (from issue #1714, previously in
//! `composio/googlecalendar_args.rs`): list queries default
//! `singleEvents=true` so recurring events expand into their
//! occurrences, and pass through the caller's `time_zone` so
//! `start.dateTime` / `end.dateTime` come back in the requester's
//! local zone rather than the calendar's default.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::openhuman::credentials::AuthService;
use crate::openhuman::oauth::persistence::GOOGLE_PROVIDER;

use crate::openhuman::providers_native::bearer::AuthedClient;

const BASE_URL: &str = "https://www.googleapis.com/calendar/v3";

/// Query parameters for [`list_events`]. RFC 3339 timestamps on the
/// time window; `time_zone` is an IANA name (e.g. `Asia/Helsinki`).
#[derive(Debug, Clone, Default)]
pub struct ListEventsQuery<'a> {
    pub calendar_id: &'a str,
    pub time_min: Option<&'a str>,
    pub time_max: Option<&'a str>,
    pub time_zone: Option<&'a str>,
    pub query: Option<&'a str>,
    pub max_results: Option<u32>,
}

/// Trimmed view of the `Events.list` response. We only deserialize the
/// fields the agent prompt + heartbeat planner currently use; unknown
/// fields are tolerated.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventsListResponse {
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub time_zone: Option<String>,
}

/// List events for `calendar_id` within `[time_min, time_max]`. Maps
/// to `GOOGLECALENDAR_EVENTS_LIST`.
pub async fn list_events(
    http: &reqwest::Client,
    service: &AuthService,
    query: &ListEventsQuery<'_>,
) -> Result<EventsListResponse> {
    let client = AuthedClient::new(http, service, GOOGLE_PROVIDER);
    let url = build_list_url(query);
    client.get_json::<EventsListResponse>(&url).await
}

/// Fetch a single event by ID. Maps to `GOOGLECALENDAR_EVENTS_GET`.
pub async fn get_event(
    http: &reqwest::Client,
    service: &AuthService,
    calendar_id: &str,
    event_id: &str,
) -> Result<Value> {
    let client = AuthedClient::new(http, service, GOOGLE_PROVIDER);
    let url = format!(
        "{BASE_URL}/calendars/{}/events/{}",
        urlencode_path(calendar_id),
        urlencode_path(event_id)
    );
    client.get_json::<Value>(&url).await
}

/// Create an event on `calendar_id`. `event` is the raw Google Calendar
/// Event resource JSON — at minimum `summary`, `start`, and `end`. Map
/// to `GOOGLECALENDAR_CREATE_EVENT`.
pub async fn create_event(
    http: &reqwest::Client,
    service: &AuthService,
    calendar_id: &str,
    event: &Value,
) -> Result<Value> {
    let client = AuthedClient::new(http, service, GOOGLE_PROVIDER);
    let url = format!(
        "{BASE_URL}/calendars/{}/events",
        urlencode_path(calendar_id)
    );
    client.post_json::<Value>(&url, event).await
}

/// Build the `events.list` URL, applying the default args identified in
/// issue #1714: `singleEvents=true` so recurring events expand, plus
/// the caller's `time_zone` so timestamps come back in the right zone.
/// Pulled out so it can be unit-tested without standing up a server.
pub(crate) fn build_list_url(q: &ListEventsQuery<'_>) -> String {
    let mut url = format!(
        "{BASE_URL}/calendars/{}/events?singleEvents=true",
        urlencode_path(q.calendar_id)
    );
    if let Some(min) = q.time_min {
        url.push_str("&timeMin=");
        url.push_str(&urlencode_q(min));
    }
    if let Some(max) = q.time_max {
        url.push_str("&timeMax=");
        url.push_str(&urlencode_q(max));
    }
    if let Some(tz) = q.time_zone {
        url.push_str("&timeZone=");
        url.push_str(&urlencode_q(tz));
    }
    if let Some(qs) = q.query {
        url.push_str("&q=");
        url.push_str(&urlencode_q(qs));
    }
    if let Some(n) = q.max_results {
        // Calendar API caps at 2500; below 1 is meaningless.
        let n = n.clamp(1, 2500);
        url.push_str(&format!("&maxResults={n}"));
    }
    url
}

/// Encode a path segment. Strict — encodes `/` and `:` which would
/// otherwise change the URL shape. Some calendar IDs (e.g. for shared
/// calendars) contain `@` and `#`, which must be percent-encoded.
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

/// Encode a query-string value. Same alphabet as `urlencode_path` —
/// kept as a separate function so a future relaxation (e.g. allowing
/// `+` for spaces) does not silently change path encoding.
fn urlencode_q(value: &str) -> String {
    urlencode_path(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_url_defaults_single_events_true() {
        let q = ListEventsQuery {
            calendar_id: "primary",
            ..Default::default()
        };
        let url = build_list_url(&q);
        assert!(
            url.contains("singleEvents=true"),
            "must default singleEvents=true: {url}"
        );
    }

    #[test]
    fn list_url_url_encodes_calendar_id_with_special_chars() {
        // Shared calendar IDs look like `user@example.com` or
        // `c_abc#def@group.calendar.google.com`. The `@`, `#`, and `.`
        // in the path must be percent-encoded.
        let q = ListEventsQuery {
            calendar_id: "user@example.com",
            ..Default::default()
        };
        let url = build_list_url(&q);
        assert!(url.contains("user%40example.com"), "got {url}");
    }

    #[test]
    fn list_url_carries_time_window_and_zone() {
        let q = ListEventsQuery {
            calendar_id: "primary",
            time_min: Some("2026-05-19T00:00:00Z"),
            time_max: Some("2026-05-20T00:00:00Z"),
            time_zone: Some("Asia/Helsinki"),
            ..Default::default()
        };
        let url = build_list_url(&q);
        assert!(url.contains("timeMin=2026-05-19T00%3A00%3A00Z"), "{url}");
        assert!(url.contains("timeMax=2026-05-20T00%3A00%3A00Z"), "{url}");
        assert!(url.contains("timeZone=Asia%2FHelsinki"), "{url}");
    }

    #[test]
    fn list_url_clamps_max_results_to_calendar_api_range() {
        let q = ListEventsQuery {
            calendar_id: "primary",
            max_results: Some(99999),
            ..Default::default()
        };
        let url = build_list_url(&q);
        assert!(url.contains("maxResults=2500"), "must clamp: {url}");

        let q = ListEventsQuery {
            calendar_id: "primary",
            max_results: Some(0),
            ..Default::default()
        };
        let url = build_list_url(&q);
        assert!(url.contains("maxResults=1"), "must clamp low: {url}");
    }

    #[test]
    fn list_url_omits_optional_params_when_unset() {
        let q = ListEventsQuery {
            calendar_id: "primary",
            ..Default::default()
        };
        let url = build_list_url(&q);
        assert!(!url.contains("timeMin"), "{url}");
        assert!(!url.contains("timeMax"), "{url}");
        assert!(!url.contains("timeZone"), "{url}");
        assert!(!url.contains("&q="), "{url}");
        assert!(!url.contains("maxResults"), "{url}");
    }
}
