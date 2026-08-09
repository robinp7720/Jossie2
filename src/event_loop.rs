use chrono::{DateTime, Utc};
use croner::Cron;
use jossie_core::types::{Message, Role};
use jossie_db::IntegrationEvent;
use jossie_server::AppState;
use jossie_server::events::{ServerEvent, persist_message};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

const INTEGRATION_POLL_INTERVAL_SECS: u64 = 120;
const BACKGROUND_WORK_INTERVAL_SECS: u64 = 5;
const PENDING_LIMIT: usize = 20;
const CALENDAR_BATCH_MAX_EVENTS: usize = 50;
const STALE_PROCESSING_EVENT_TIMEOUT_SECS: i64 = 60 * 60;
const HEARTBEAT_SETTINGS_NAMESPACE: &str = "heartbeat";
const HEARTBEAT_LAST_RUN_KEY: &str = "last_run_at";
const HEARTBEAT_UPCOMING_WINDOW_HOURS: i64 = 24;
const HEARTBEAT_EVENT_TYPE: &str = "heartbeat_check";

include!("event_loop/worker.rs");
include!("event_loop/events.rs");
include!("event_loop/scheduler.rs");
include!("event_loop/heartbeat.rs");
#[cfg(test)]
mod tests {
    use super::*;
    include!("event_loop/tests.rs");
}
