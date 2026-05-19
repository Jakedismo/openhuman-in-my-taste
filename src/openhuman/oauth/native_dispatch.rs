//! Slug-to-native-function dispatch table. The bridge between the
//! Composio-shaped agent surface (which addresses operations by the
//! uppercase slugs `GMAIL_SEND_EMAIL`, `GOOGLECALENDAR_EVENTS_LIST`,
//! …) and the typed Rust functions in
//! [`crate::openhuman::providers_native`].
//!
//! The intent for Phase 5 cutover: every call site that today hits
//! Composio's `execute` route runs `try_dispatch_native` first; if it
//! returns `Some(_)`, the native path handled the request and the
//! Composio call is skipped entirely. `None` means "no native impl —
//! fall through" so partial coverage during the build-new-beside-old
//! window is safe.
//!
//! The feature flag is read from the environment for now
//! (`OPENHUMAN_NATIVE_OAUTH=1`); promoting it to a typed
//! `config.feature.native_oauth_enabled` field is a follow-up.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::openhuman::credentials::AuthService;
use crate::openhuman::providers_native::{
    github as gh_native,
    google::{self, calendar::ListEventsQuery},
};

/// Environment variable that enables routing to native providers. Set
/// to `1` to flip dispatch on; any other value (or unset) means the
/// dispatcher always returns `None` and Composio handles everything.
pub const ENABLE_ENV: &str = "OPENHUMAN_NATIVE_OAUTH";

/// True iff [`ENABLE_ENV`] is set to `1`.
pub fn is_enabled() -> bool {
    std::env::var(ENABLE_ENV).as_deref() == Ok("1")
}

/// Try to dispatch `tool` to a native client. Returns:
///   * `None` — no native impl for this slug, caller should fall
///     through to Composio.
///   * `Some(Ok(json))` — native handled the request, here is the
///     payload (shape mirrors what Composio's `data` field would
///     carry).
///   * `Some(Err(msg))` — native handled but failed; caller should
///     surface this verbatim instead of retrying through Composio
///     (the error is authoritative).
///
/// Returns `None` when the feature flag is off so callers never see
/// a partial-rollout footgun even if a slug ships native.
pub async fn try_dispatch_native(
    http: &reqwest::Client,
    service: &AuthService,
    tool: &str,
    arguments: Option<&Value>,
) -> Option<Result<Value>> {
    if !is_enabled() {
        return None;
    }
    let trimmed = tool.trim();
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    match trimmed {
        "GMAIL_SEND_EMAIL" => Some(dispatch_gmail_send(http, service, &args).await),
        "GMAIL_FETCH_EMAILS" => Some(dispatch_gmail_list(http, service, &args).await),
        "GMAIL_DELETE_EMAIL" => Some(dispatch_gmail_delete(http, service, &args).await),
        "GMAIL_ADD_LABEL_TO_EMAIL" => Some(dispatch_gmail_add_label(http, service, &args).await),
        "GOOGLECALENDAR_EVENTS_LIST" | "GOOGLECALENDAR_FIND_EVENT" => {
            Some(dispatch_calendar_list(http, service, &args).await)
        }
        "GOOGLECALENDAR_EVENTS_GET" => Some(dispatch_calendar_get(http, service, &args).await),
        "GOOGLECALENDAR_CREATE_EVENT" => Some(dispatch_calendar_create(http, service, &args).await),
        "GITHUB_USERS_GET_AUTHENTICATED" => {
            Some(dispatch_github_get_authenticated(http, service).await)
        }
        _ => None,
    }
}

async fn dispatch_gmail_send(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let recipient = str_field(args, "recipient_email").or_else(|_| str_field(args, "to"))?;
    let subject = str_field(args, "subject").unwrap_or_default();
    let body = str_field(args, "body")
        .or_else(|_| str_field(args, "text"))
        .unwrap_or_default();

    let msg = google::gmail::send_message(http, service, &recipient, &subject, &body).await?;
    Ok(json!({
        "id": msg.id,
        "thread_id": msg.thread_id,
    }))
}

async fn dispatch_gmail_list(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let query = str_field(args, "query")
        .or_else(|_| str_field(args, "q"))
        .ok();
    let max_results = args
        .get("max_results")
        .or_else(|| args.get("maxResults"))
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(u32::MAX as u64) as u32;

    let resp = google::gmail::list_messages(http, service, query.as_deref(), max_results).await?;
    Ok(json!({
        "messages": resp.messages,
        "result_size_estimate": resp.result_size_estimate,
        "next_page_token": resp.next_page_token,
    }))
}

async fn dispatch_gmail_delete(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let message_id = str_field(args, "message_id").or_else(|_| str_field(args, "id"))?;
    google::gmail::delete_message(http, service, &message_id).await?;
    Ok(json!({ "deleted": true, "message_id": message_id }))
}

async fn dispatch_gmail_add_label(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let message_id = str_field(args, "message_id").or_else(|_| str_field(args, "id"))?;
    // Composio's arg shape uses `label_ids: [String]`; pick the first
    // for the single-label native API. Callers wanting bulk labelling
    // can call us repeatedly until that API surfaces a multi-label
    // path.
    let label_id = args
        .get("label_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            args.get("label_ids")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| {
            anyhow!("native dispatch: missing 'label_id' or non-empty 'label_ids' for GMAIL_ADD_LABEL_TO_EMAIL")
        })?;
    let msg = google::gmail::add_label(http, service, &message_id, &label_id).await?;
    Ok(json!({
        "id": msg.id,
        "thread_id": msg.thread_id,
        "added_label_id": label_id,
    }))
}

fn calendar_id_or_primary(args: &Value) -> String {
    args.get("calendar_id")
        .or_else(|| args.get("calendarId"))
        .and_then(Value::as_str)
        .unwrap_or("primary")
        .to_string()
}

async fn dispatch_calendar_list(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let calendar_id = calendar_id_or_primary(args);
    let time_min = args
        .get("time_min")
        .or_else(|| args.get("timeMin"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let time_max = args
        .get("time_max")
        .or_else(|| args.get("timeMax"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let time_zone = args
        .get("time_zone")
        .or_else(|| args.get("timeZone"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let query = args
        .get("q")
        .or_else(|| args.get("query"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let max_results = args
        .get("max_results")
        .or_else(|| args.get("maxResults"))
        .and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32);

    let q = ListEventsQuery {
        calendar_id: &calendar_id,
        time_min: time_min.as_deref(),
        time_max: time_max.as_deref(),
        time_zone: time_zone.as_deref(),
        query: query.as_deref(),
        max_results,
    };
    let resp = google::calendar::list_events(http, service, &q).await?;
    Ok(json!({
        "items": resp.items,
        "next_page_token": resp.next_page_token,
        "time_zone": resp.time_zone,
    }))
}

async fn dispatch_calendar_get(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let calendar_id = calendar_id_or_primary(args);
    let event_id = str_field(args, "event_id").or_else(|_| str_field(args, "eventId"))?;
    google::calendar::get_event(http, service, &calendar_id, &event_id).await
}

async fn dispatch_calendar_create(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let calendar_id = calendar_id_or_primary(args);
    // Composio passes the event body as the args object minus the
    // `calendar_id` key. Strip it so the API doesn't see an unexpected
    // field.
    let mut body = args.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.remove("calendar_id");
        obj.remove("calendarId");
    }
    google::calendar::create_event(http, service, &calendar_id, &body).await
}

async fn dispatch_github_get_authenticated(
    http: &reqwest::Client,
    service: &AuthService,
) -> Result<Value> {
    let user = gh_native::get_authenticated_user(http, service).await?;
    Ok(json!({
        "login": user.login,
        "id": user.id,
        "name": user.name,
        "email": user.email,
        "html_url": user.html_url,
    }))
}

/// Extract a string field, erroring with a clear message if missing or
/// wrong type. Used so dispatch errors point at the offending arg
/// rather than a generic decode failure.
fn str_field(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("native dispatch: missing or non-string arg '{key}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: the env-var-driven `is_enabled()` gate is intentionally
    // not tested in unit tests — `std::env` is process-global, and
    // cargo's default parallel test runner races concurrent set/unset
    // pairs into false negatives. The fall-through behavior (flag off
    // OR unknown slug → caller proceeds with Composio) is exercised
    // by the existing 459 composio integration tests, none of which
    // set OPENHUMAN_NATIVE_OAUTH=1, so the dispatcher never short-
    // circuits and Composio handles everything. The flag-ON path is
    // validated manually via the `oauth-connect` binary and a real
    // provider during Phase 4.

    #[test]
    fn str_field_extracts_string() {
        let v = json!({"recipient_email": "a@b.com"});
        assert_eq!(str_field(&v, "recipient_email").unwrap(), "a@b.com");
    }

    #[test]
    fn str_field_errors_with_arg_name_in_message() {
        let v = json!({});
        let err = str_field(&v, "recipient_email").unwrap_err();
        assert!(
            err.to_string().contains("'recipient_email'"),
            "error should name the missing arg: {err}"
        );
    }

    #[test]
    fn str_field_errors_on_non_string_type() {
        let v = json!({"recipient_email": 42});
        assert!(str_field(&v, "recipient_email").is_err());
    }
}
