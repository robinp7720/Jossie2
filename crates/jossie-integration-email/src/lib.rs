use chrono::Utc;
use jossie_core::config::EmailConfig;
use jossie_core::integration::{Integration, OnboardingField, OnboardingStatus, ToolDefinition};
use jossie_db::Database;
use jossie_db::IntegrationAccount;
use mailparse::{DispositionType, MailHeaderMap, ParsedMail};
use serde_json::Value;
use std::sync::Arc;

type ImapSession = async_imap::Session<
    async_native_tls::TlsStream<tokio_util::compat::Compat<tokio::net::TcpStream>>,
>;

const DEFAULT_FOLDER: &str = "INBOX";
const MAX_EMAIL_BODY_CHARS: usize = 60_000;
const MAX_FALLBACK_PREVIEW_CHARS: usize = 4_000;
const EMAIL_INTEGRATION: &str = "email";
const MAX_POLL_FETCH_UIDS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
enum MailboxPollAction {
    SeedCursor { last_seen_uid: u32 },
    PollFrom { start_uid: u32 },
    NoChange,
}

#[derive(Debug, Clone)]
struct PollAccount {
    id: String,
    email: String,
    config: EmailConfig,
}

#[derive(Debug, Clone)]
struct ImapEventSummary {
    uid: u32,
    message_unique_id: String,
    header_message_id: Option<String>,
    from: String,
    to: Vec<String>,
    subject: String,
    date: String,
}

#[derive(Debug, Clone, Default)]
pub struct EmailSearchRequest {
    pub query: Option<String>,
    pub terms: Vec<String>,
    pub match_mode: String,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub max_results: Option<u32>,
    pub page_token: Option<String>,
    pub folder: Option<String>,
}

pub struct EmailIntegration {
    default_config: Option<EmailConfig>,
    db: Option<Arc<Database>>,
}

#[derive(Debug, Clone)]
pub struct EmailAttachment {
    pub part_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct EmailMessageContent {
    pub uid: u32,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub date: String,
    pub body: String,
    pub body_source: String,
    pub attachments: Vec<EmailAttachment>,
}

include!("email/polling.rs");
include!("email/provider.rs");
include!("email/integration.rs");
include!("email/tests.rs");
