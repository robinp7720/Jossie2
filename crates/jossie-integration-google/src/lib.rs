use chrono::{DateTime, Duration, Utc};
use jossie_core::config::GoogleConfig;
use jossie_core::events::{CALENDAR_EVENT_UPDATED, GMAIL_NEW_MESSAGE};
use jossie_core::integration::{
    EmptyToolArgs, Integration, OnboardingField, OnboardingStatus, ToolDefinition,
};
use jossie_db::Database;
use jossie_db::IntegrationAccount;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct GoogleIntegration {
    config: GoogleConfig,
    client: reqwest::Client,
    tokens: Arc<RwLock<HashMap<String, TokenData>>>,
    db: Option<Arc<Database>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DriveSearchArgs {
    account_id: String,
    query: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DriveReadArgs {
    account_id: String,
    file_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DriveListArgs {
    account_id: String,
    #[schemars(required)]
    folder_id: Option<String>,
    #[schemars(required)]
    query: Option<String>,
    #[schemars(required)]
    page_size: Option<u32>,
    #[schemars(required)]
    page_token: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GoogleAccountArgs {
    account_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContactsSearchArgs {
    account_id: String,
    query: String,
    #[schemars(required)]
    page_size: Option<u32>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContactReadArgs {
    account_id: String,
    resource_name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CalendarListEventsArgs {
    account_id: String,
    #[schemars(required)]
    calendar_id: Option<String>,
    query: String,
    time_min: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CalendarCreateEventArgs {
    account_id: String,
    #[schemars(required)]
    calendar_id: Option<String>,
    summary: String,
    start_time: String,
    end_time: String,
    description: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CalendarUpdateEventArgs {
    account_id: String,
    #[schemars(required)]
    calendar_id: Option<String>,
    event_id: String,
    #[schemars(required)]
    summary: Option<String>,
    #[schemars(required)]
    start_time: Option<String>,
    #[schemars(required)]
    end_time: Option<String>,
    #[schemars(required)]
    start_date: Option<String>,
    #[schemars(required)]
    end_date: Option<String>,
    #[schemars(required)]
    description: Option<String>,
    #[schemars(required)]
    location: Option<String>,
    #[schemars(required)]
    send_updates: Option<String>,
}

const GOOGLE_INTEGRATION: &str = "google";
const ACCOUNT_STATUS_PAUSED_INVALID_GRANT: &str = "paused_invalid_grant";
const RECONNECT_NOTICE_COOLDOWN_HOURS: i64 = 24;

#[derive(Clone)]
struct TokenData {
    access_token: String,
    expires_at: std::time::Instant,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct StoredAccount {
    refresh_token: String,
    #[serde(default)]
    email: String,
}

#[derive(Debug, Clone)]
pub struct GmailProfile {
    pub history_id: String,
}

#[derive(Debug, Clone)]
pub struct GmailMessageSummary {
    pub id: String,
    pub thread_id: String,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub snippet: String,
    pub received_at: String,
    pub internal_ts_ms: i64,
}

#[derive(Debug, Clone)]
pub struct GmailHistoryPollResult {
    pub history_id: String,
    pub messages: Vec<GmailMessageSummary>,
}

#[derive(Debug, Clone)]
pub enum GmailHistoryOutcome {
    Updated(GmailHistoryPollResult),
    Reset { history_id: String },
}

#[derive(Debug, Clone)]
pub struct CalendarEventSummary {
    pub id: String,
    pub summary: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub status: String,
    pub updated: String,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarListEntry {
    pub id: String,
    pub summary: String,
    pub description: Option<String>,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CalendarEventUpdate {
    summary: Option<String>,
    start_time: Option<String>,
    end_time: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    description: Option<String>,
    location: Option<String>,
}

include!("google/auth_accounts.rs");
include!("google/gmail_provider.rs");
include!("google/calendar_provider.rs");
include!("google/mail.rs");
include!("google/drive.rs");
include!("google/calendar.rs");
include!("google/personal.rs");
include!("google/polling.rs");
include!("google/payload.rs");
include!("google/integration.rs");
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_base64_decoder_preserves_binary_attachment_bytes() {
        assert_eq!(decode_base64_url_bytes("AAEC_w"), Some(vec![0, 1, 2, 255]));
    }

    #[test]
    fn calendar_update_body_builds_timed_patch() {
        let body = build_calendar_update_body(CalendarEventUpdate {
            summary: Some("Project sync".to_string()),
            start_time: Some("2026-05-01T10:00:00+02:00".to_string()),
            end_time: Some("2026-05-01T10:30:00+02:00".to_string()),
            description: Some("".to_string()),
            location: Some("Room 4".to_string()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(body["summary"], "Project sync");
        assert_eq!(body["start"]["dateTime"], "2026-05-01T10:00:00+02:00");
        assert_eq!(body["end"]["dateTime"], "2026-05-01T10:30:00+02:00");
        assert_eq!(body["description"], "");
        assert_eq!(body["location"], "Room 4");
    }

    #[test]
    fn calendar_update_body_builds_all_day_patch() {
        let body = build_calendar_update_body(CalendarEventUpdate {
            start_date: Some("2026-05-01".to_string()),
            end_date: Some("2026-05-02".to_string()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(body["start"]["date"], "2026-05-01");
        assert_eq!(body["end"]["date"], "2026-05-02");
    }

    #[test]
    fn calendar_update_body_rejects_empty_patch() {
        let err = build_calendar_update_body(CalendarEventUpdate::default()).unwrap_err();
        assert!(err.to_string().contains("At least one"));
    }

    #[test]
    fn calendar_update_body_rejects_partial_time_change() {
        let err = build_calendar_update_body(CalendarEventUpdate {
            start_time: Some("2026-05-01T10:00:00+02:00".to_string()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("end_time is required"));
    }

    #[test]
    fn calendar_update_body_rejects_mixed_timed_and_all_day_change() {
        let err = build_calendar_update_body(CalendarEventUpdate {
            start_time: Some("2026-05-01T10:00:00+02:00".to_string()),
            end_time: Some("2026-05-01T10:30:00+02:00".to_string()),
            start_date: Some("2026-05-01".to_string()),
            end_date: Some("2026-05-02".to_string()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("Use either"));
    }

    #[test]
    fn calendar_send_updates_defaults_to_none_and_validates_values() {
        assert_eq!(
            normalize_calendar_send_updates(None).unwrap(),
            Some("none".to_string())
        );
        assert_eq!(
            normalize_calendar_send_updates(Some("externalOnly")).unwrap(),
            Some("externalOnly".to_string())
        );
        assert!(normalize_calendar_send_updates(Some("invalid")).is_err());
    }

    #[test]
    fn google_tools_include_calendar_update_event() {
        let integration = GoogleIntegration::new(&GoogleConfig::default());
        let tools = integration.tools();
        let tool = tools
            .iter()
            .find(|tool| tool.name == "calendar_update_event")
            .expect("calendar_update_event tool should be registered");

        assert_eq!(
            tool.parameters["properties"]["event_id"]["type"],
            serde_json::json!("string")
        );
        assert!(
            tool.parameters["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("send_updates"))
        );
        assert!(tools.iter().all(|tool| !tool.name.starts_with("gmail_")));
    }
}
