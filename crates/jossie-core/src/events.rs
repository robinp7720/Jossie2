//! Stable names and broad categories for integration events.
//!
//! Events remain stored as strings so existing databases and unknown future
//! integrations stay compatible. Code that branches on known events should use
//! this module instead of repeating string literals.

pub const NEW_EMAIL: &str = "new_email";
pub const GMAIL_NEW_MESSAGE: &str = "gmail_new_message";
pub const NEW_EMAIL_BATCH: &str = "new_email_batch";
pub const CALENDAR_EVENT: &str = "calendar_event";
pub const CALENDAR_EVENT_UPDATED: &str = "calendar_event_updated";
pub const CALENDAR_EVENT_BATCH: &str = "calendar_event_batch";
pub const HEARTBEAT_CHECK: &str = "heartbeat_check";
pub const TASK_DUE: &str = "task_due";
pub const TASK_CHANGED: &str = "task_changed";
pub const HOME_STATE_CHANGED: &str = "home_state_changed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationEventKind {
    Email,
    Calendar,
    Heartbeat,
    Task,
    Home,
    Other,
}

pub fn integration_event_kind(event_type: &str) -> IntegrationEventKind {
    match event_type {
        NEW_EMAIL | GMAIL_NEW_MESSAGE | NEW_EMAIL_BATCH => IntegrationEventKind::Email,
        CALENDAR_EVENT | CALENDAR_EVENT_UPDATED | CALENDAR_EVENT_BATCH => {
            IntegrationEventKind::Calendar
        }
        HEARTBEAT_CHECK => IntegrationEventKind::Heartbeat,
        TASK_DUE | TASK_CHANGED => IntegrationEventKind::Task,
        HOME_STATE_CHANGED => IntegrationEventKind::Home,
        _ => IntegrationEventKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_event_names_have_one_shared_classification() {
        assert_eq!(
            integration_event_kind(NEW_EMAIL),
            IntegrationEventKind::Email
        );
        assert_eq!(
            integration_event_kind(CALENDAR_EVENT_UPDATED),
            IntegrationEventKind::Calendar
        );
        assert_eq!(
            integration_event_kind(HEARTBEAT_CHECK),
            IntegrationEventKind::Heartbeat
        );
        assert_eq!(
            integration_event_kind("custom"),
            IntegrationEventKind::Other
        );
    }
}
