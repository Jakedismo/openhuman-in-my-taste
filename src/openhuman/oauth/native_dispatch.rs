//! Slug-to-native-function dispatch table. The bridge between the
//! Composio-shaped agent surface (which addresses operations by the
//! uppercase slugs `GMAIL_SEND_EMAIL`, `GOOGLECALENDAR_EVENTS_LIST`,
//! …) and the typed Rust functions in
//! [`crate::openhuman::providers_native`].
//!
//! As of Phase 5.1 native dispatch is the ONLY execution route — the
//! Composio backend fall-through was removed from
//! [`crate::openhuman::composio::ops::composio_execute`]. A slug
//! without a native arm hard-errors at the call site. Add new arms
//! to the match below to extend coverage.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::openhuman::credentials::AuthService;
use crate::openhuman::providers_native::{
    github as gh_native,
    google::{self, calendar::ListEventsQuery},
};

/// Try to dispatch `tool` to a native client. Returns:
///   * `None` — no native impl for this slug; caller hard-errors
///     (the Composio fall-through was removed in Phase 5.1).
///   * `Some(Ok(json))` — native handled the request, here is the
///     payload (shape mirrors what Composio's `data` field would
///     carry).
///   * `Some(Err(msg))` — native handled but failed; caller should
///     surface this verbatim. The error is authoritative.
pub async fn try_dispatch_native(
    http: &reqwest::Client,
    service: &AuthService,
    tool: &str,
    arguments: Option<&Value>,
) -> Option<Result<Value>> {
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
        "GOOGLEDRIVE_LIST_FILES" | "GOOGLEDRIVE_FIND_FILE" => {
            Some(dispatch_drive_list(http, service, &args).await)
        }
        "GOOGLEDRIVE_GET_FILE_METADATA" => Some(dispatch_drive_get(http, service, &args).await),
        "GOOGLEDRIVE_CREATE_FILE" | "GOOGLEDRIVE_CREATE_FILE_FROM_TEXT" => {
            Some(dispatch_drive_create(http, service, &args).await)
        }
        "GITHUB_USERS_GET_AUTHENTICATED" => {
            Some(dispatch_github_get_authenticated(http, service).await)
        }
        "GITHUB_CREATE_AN_ISSUE" => Some(dispatch_github_create_issue(http, service, &args).await),
        "GITHUB_LIST_REPOSITORIES_FOR_THE_AUTHENTICATED_USER" => {
            Some(dispatch_github_list_repos(http, service, &args).await)
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

async fn dispatch_drive_list(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let query = str_field(args, "query")
        .or_else(|_| str_field(args, "q"))
        .ok();
    let page_size = args
        .get("page_size")
        .or_else(|| args.get("pageSize"))
        .and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32);
    let resp = google::drive::list_files(http, service, query.as_deref(), page_size).await?;
    Ok(json!({
        "files": resp.files,
        "next_page_token": resp.next_page_token,
    }))
}

async fn dispatch_drive_get(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let file_id = str_field(args, "file_id").or_else(|_| str_field(args, "fileId"))?;
    let file = google::drive::get_file_metadata(http, service, &file_id).await?;
    Ok(serde_json::to_value(file)?)
}

async fn dispatch_drive_create(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    // Composio passes the file body either as an opaque metadata object
    // or as separate `name` / `mimeType` / `parents` fields. Honor both
    // shapes — if a `metadata` object is present, use it verbatim;
    // otherwise rebuild it from top-level args.
    let metadata = match args.get("metadata") {
        Some(v) if v.is_object() => v.clone(),
        _ => {
            let mut obj = serde_json::Map::new();
            for key in ["name", "mimeType", "mime_type", "parents", "description"] {
                if let Some(v) = args.get(key) {
                    let canonical = if key == "mime_type" { "mimeType" } else { key };
                    obj.insert(canonical.to_string(), v.clone());
                }
            }
            Value::Object(obj)
        }
    };
    let file = google::drive::create_file_metadata(http, service, &metadata).await?;
    Ok(serde_json::to_value(file)?)
}

async fn dispatch_github_create_issue(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let owner = str_field(args, "owner")?;
    let repo = str_field(args, "repo")?;
    let title = str_field(args, "title")?;
    let body = args.get("body").and_then(Value::as_str);
    let issue = gh_native::create_issue(http, service, &owner, &repo, &title, body).await?;
    Ok(json!({
        "id": issue.id,
        "number": issue.number,
        "title": issue.title,
        "html_url": issue.html_url,
        "state": issue.state,
        "body": issue.body,
    }))
}

async fn dispatch_github_list_repos(
    http: &reqwest::Client,
    service: &AuthService,
    args: &Value,
) -> Result<Value> {
    let per_page = args
        .get("per_page")
        .or_else(|| args.get("perPage"))
        .and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32);
    let repos = gh_native::list_authenticated_repos(http, service, per_page).await?;
    Ok(json!({ "repositories": repos }))
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

    // The dispatcher's match arms are exercised through the integration
    // entry point `composio_execute`; per-arm wiring (correct provider
    // client call, arg-shape mapping) is covered by the provider-native
    // test suites in `providers_native::{google,github}`. Unknown-slug
    // fallthrough is covered by `composio_execute_errors_for_unknown_slug`
    // in `composio/ops_test.rs`.

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
