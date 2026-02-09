use chrono::{DateTime, Duration, Utc};
use jossie_core::config::GoogleConfig;
use jossie_core::integration::{Integration, OnboardingField, OnboardingStatus, ToolDefinition};
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

impl GoogleIntegration {
    pub fn new(config: &GoogleConfig) -> Self {
        Self {
            config: config.clone(),
            client: reqwest::Client::new(),
            tokens: Arc::new(RwLock::new(HashMap::new())),
            db: None,
        }
    }

    pub fn set_db(&mut self, db: Arc<Database>) {
        self.db = Some(db);
    }

    fn account_status_key(account_id: &str) -> String {
        format!("account_status:{account_id}")
    }

    fn account_status_detail_key(account_id: &str) -> String {
        format!("account_status_detail:{account_id}")
    }

    fn account_paused_refresh_key(account_id: &str) -> String {
        format!("account_paused_refresh_token:{account_id}")
    }

    fn account_last_reconnect_notice_key(account_id: &str) -> String {
        format!("last_reconnect_notice_at:{account_id}")
    }

    fn is_invalid_grant_text(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("invalid_grant")
            && (lower.contains("token refresh failed")
                || lower.contains("token has been expired or revoked"))
    }

    fn get_account_refresh_token(acc: &IntegrationAccount) -> Option<String> {
        serde_json::from_str::<StoredAccount>(&acc.data)
            .ok()
            .map(|data| data.refresh_token)
            .filter(|token| !token.trim().is_empty())
    }

    async fn clear_account_pause_state(
        &self,
        db: &Arc<Database>,
        account_id: &str,
    ) -> anyhow::Result<()> {
        db.set_integration_setting(
            GOOGLE_INTEGRATION,
            &Self::account_status_key(account_id),
            "active",
        )
        .await?;
        db.set_integration_setting(
            GOOGLE_INTEGRATION,
            &Self::account_status_detail_key(account_id),
            "",
        )
        .await?;
        db.set_integration_setting(
            GOOGLE_INTEGRATION,
            &Self::account_paused_refresh_key(account_id),
            "",
        )
        .await?;
        Ok(())
    }

    async fn is_account_paused(
        &self,
        db: &Arc<Database>,
        acc: &IntegrationAccount,
    ) -> anyhow::Result<bool> {
        let status = db
            .get_integration_setting(GOOGLE_INTEGRATION, &Self::account_status_key(&acc.id))
            .await?;
        if status.as_deref() != Some(ACCOUNT_STATUS_PAUSED_INVALID_GRANT) {
            return Ok(false);
        }

        // If refresh token changed since pause, resume polling automatically.
        let paused_refresh = db
            .get_integration_setting(
                GOOGLE_INTEGRATION,
                &Self::account_paused_refresh_key(&acc.id),
            )
            .await?;
        let current_refresh = Self::get_account_refresh_token(acc);
        if let (Some(paused), Some(current)) = (paused_refresh, current_refresh) {
            if !paused.is_empty() && paused != current {
                tracing::info!(
                    "Google account {} refresh token changed; clearing paused status",
                    acc.id
                );
                self.clear_account_pause_state(db, &acc.id).await?;
                return Ok(false);
            }
        }

        Ok(true)
    }

    async fn pause_account_invalid_grant(
        &self,
        db: &Arc<Database>,
        acc: &IntegrationAccount,
        error_message: &str,
    ) -> anyhow::Result<()> {
        db.set_integration_setting(
            GOOGLE_INTEGRATION,
            &Self::account_status_key(&acc.id),
            ACCOUNT_STATUS_PAUSED_INVALID_GRANT,
        )
        .await?;

        let detail = serde_json::json!({
            "reason": ACCOUNT_STATUS_PAUSED_INVALID_GRANT,
            "paused_at": Utc::now().to_rfc3339(),
            "last_error": error_message,
        });
        db.set_integration_setting(
            GOOGLE_INTEGRATION,
            &Self::account_status_detail_key(&acc.id),
            &detail.to_string(),
        )
        .await?;

        let refresh = Self::get_account_refresh_token(acc).unwrap_or_default();
        db.set_integration_setting(
            GOOGLE_INTEGRATION,
            &Self::account_paused_refresh_key(&acc.id),
            &refresh,
        )
        .await?;

        self.tokens.write().await.remove(&acc.id);
        Ok(())
    }

    async fn queue_reconnect_notice_if_due(
        &self,
        db: &Arc<Database>,
        acc: &IntegrationAccount,
    ) -> anyhow::Result<()> {
        let last_notice = db
            .get_integration_setting(
                GOOGLE_INTEGRATION,
                &Self::account_last_reconnect_notice_key(&acc.id),
            )
            .await?;
        if let Some(last_notice) = last_notice {
            if let Ok(last_dt) = DateTime::parse_from_rfc3339(&last_notice) {
                let cooldown = Duration::hours(RECONNECT_NOTICE_COOLDOWN_HOURS);
                if Utc::now() - last_dt.with_timezone(&Utc) < cooldown {
                    return Ok(());
                }
            }
        }

        let Some(chat) = db.get_latest_telegram_chat().await? else {
            return Ok(());
        };

        let message = format!(
            "Google account '{}' needs reconnect: refresh token was expired/revoked. Please reconnect it in settings.",
            acc.name
        );
        db.queue_oob_message(chat.conversation_id, &message, "high")
            .await?;
        db.set_integration_setting(
            GOOGLE_INTEGRATION,
            &Self::account_last_reconnect_notice_key(&acc.id),
            &Utc::now().to_rfc3339(),
        )
        .await?;
        Ok(())
    }

    pub fn generate_auth_url(
        config: &GoogleConfig,
        redirect_uri: &str,
        state: Option<&str>,
    ) -> String {
        let scopes = [
            "https://mail.google.com/",
            "https://www.googleapis.com/auth/drive",
            "https://www.googleapis.com/auth/gmail.send",
            "https://www.googleapis.com/auth/calendar",
        ]
        .join(" ");

        let mut url = format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
            config.client_id, redirect_uri, scopes
        );

        if let Some(state) = state {
            url.push_str("&state=");
            url.push_str(state);
        }

        url
    }

    pub async fn exchange_code(
        config: &GoogleConfig,
        code: &str,
        redirect_uri: &str,
    ) -> anyhow::Result<String> {
        let client = reqwest::Client::new();
        let resp = client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", &config.client_id),
                ("client_secret", &config.client_secret),
                ("code", &code.to_string()),
                ("grant_type", &"authorization_code".to_string()),
                ("redirect_uri", &redirect_uri.to_string()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed: {body}");
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            refresh_token: Option<String>,
        }

        let tr: TokenResponse = resp.json().await?;
        tr.refresh_token.ok_or_else(|| anyhow::anyhow!("No refresh token in response (did you already authorize? Try revoking access first)"))
    }

    async fn get_refresh_token(&self, account_id: &str) -> anyhow::Result<String> {
        if account_id.trim().is_empty() {
            anyhow::bail!("account_id is required");
        }

        if let Some(db) = &self.db {
            if let Some(acc) = db.get_integration_account(account_id).await? {
                let stored: StoredAccount = serde_json::from_str(&acc.data)?;
                if stored.refresh_token.trim().is_empty() {
                    anyhow::bail!("Refresh token is missing for account: {}", account_id);
                }
                return Ok(stored.refresh_token);
            }
            anyhow::bail!("Account not found: {}", account_id);
        }

        anyhow::bail!("Google integration database not configured")
    }

    async fn get_access_token(&self, account_id: &str) -> anyhow::Result<String> {
        let account_key = account_id.trim().to_string();
        if account_key.is_empty() {
            anyhow::bail!("account_id is required");
        }

        // Check cached token
        {
            let tokens = self.tokens.read().await;
            if let Some(td) = tokens.get(&account_key) {
                if td.expires_at > std::time::Instant::now() {
                    return Ok(td.access_token.clone());
                }
            }
        }

        let refresh_token = self.get_refresh_token(&account_key).await?;

        // Refresh token
        let resp = self
            .client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", &self.config.client_id),
                ("client_secret", &self.config.client_secret),
                ("refresh_token", &refresh_token),
                ("grant_type", &"refresh_token".to_string()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed for account {}: {}", account_key, body);
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: u64,
        }

        let tr: TokenResponse = resp.json().await?;
        let td = TokenData {
            access_token: tr.access_token.clone(),
            expires_at: std::time::Instant::now()
                + std::time::Duration::from_secs(tr.expires_in.saturating_sub(60)),
        };

        self.tokens.write().await.insert(account_key, td.clone());
        Ok(td.access_token)
    }

    async fn list_accounts(&self) -> anyhow::Result<String> {
        println!("Listing Google integration accounts");
        let mut accounts = Vec::new();

        if let Some(db) = &self.db {
            let db_accounts = db.list_integration_accounts("google").await?;
            for acc in db_accounts {
                println!("Listing Google account: {} - {}", acc.id, acc.name);
                let email = if let Ok(data) = serde_json::from_str::<StoredAccount>(&acc.data) {
                    data.email
                } else {
                    "unknown".to_string()
                };

                accounts.push(serde_json::json!({
                    "id": acc.id,
                    "name": acc.name,
                    "email": email,
                    "type": "db"
                }));
            }
        }
        Ok(serde_json::to_string_pretty(&accounts)?)
    }

    pub async fn gmail_get_profile(&self, account_id: &str) -> anyhow::Result<GmailProfile> {
        let token = self.get_access_token(account_id).await?;
        let resp = self
            .client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/profile")
            .bearer_auth(&token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail profile fetch failed: {body}");
        }

        #[derive(Deserialize)]
        struct ProfileResp {
            #[serde(rename = "historyId")]
            history_id: String,
        }

        let profile: ProfileResp = resp.json().await?;
        Ok(GmailProfile {
            history_id: profile.history_id,
        })
    }

    pub async fn gmail_list_history(
        &self,
        account_id: &str,
        start_history_id: &str,
    ) -> anyhow::Result<GmailHistoryOutcome> {
        let token = self.get_access_token(account_id).await?;
        let mut page_token: Option<String> = None;
        let mut message_ids: HashSet<String> = HashSet::new();
        let mut latest_history_id: Option<String> = None;

        loop {
            let mut req = self
                .client
                .get("https://gmail.googleapis.com/gmail/v1/users/me/history")
                .bearer_auth(&token)
                .query(&[
                    ("startHistoryId", start_history_id),
                    ("historyTypes", "messageAdded"),
                    ("maxResults", "100"),
                ]);

            if let Some(ref token) = page_token {
                req = req.query(&[("pageToken", token)]);
            }

            let resp = req.send().await?;
            if resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::BAD_REQUEST {
                let profile = self.gmail_get_profile(account_id).await?;
                return Ok(GmailHistoryOutcome::Reset {
                    history_id: profile.history_id,
                });
            }

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Gmail history list failed: {body}");
            }

            #[derive(Deserialize)]
            struct HistoryList {
                #[serde(rename = "historyId")]
                history_id: Option<String>,
                #[serde(rename = "nextPageToken")]
                next_page_token: Option<String>,
                #[serde(default)]
                history: Vec<HistoryItem>,
            }

            #[derive(Deserialize)]
            struct HistoryItem {
                #[serde(rename = "messagesAdded")]
                messages_added: Option<Vec<MessageAdded>>,
            }

            #[derive(Deserialize)]
            struct MessageAdded {
                message: Option<MessageRef>,
            }

            #[derive(Deserialize)]
            struct MessageRef {
                id: String,
            }

            let list: HistoryList = resp.json().await?;
            if let Some(hid) = list.history_id {
                latest_history_id = Some(hid);
            }

            for item in list.history {
                if let Some(added) = item.messages_added {
                    for entry in added {
                        if let Some(message) = entry.message {
                            message_ids.insert(message.id);
                        }
                    }
                }
            }

            if let Some(next) = list.next_page_token {
                page_token = Some(next);
            } else {
                break;
            }
        }

        let mut messages = Vec::new();
        for message_id in message_ids {
            if let Ok(summary) = self
                .gmail_fetch_message_summary(account_id, &message_id)
                .await
            {
                messages.push(summary);
            }
        }

        Ok(GmailHistoryOutcome::Updated(GmailHistoryPollResult {
            history_id: latest_history_id.unwrap_or_else(|| start_history_id.to_string()),
            messages,
        }))
    }

    async fn gmail_fetch_message_summary(
        &self,
        account_id: &str,
        message_id: &str,
    ) -> anyhow::Result<GmailMessageSummary> {
        let token = self.get_access_token(account_id).await?;
        let resp = self
            .client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}"
            ))
            .bearer_auth(&token)
            .query(&[
                ("format", "metadata"),
                ("metadataHeaders", "From"),
                ("metadataHeaders", "Subject"),
                ("metadataHeaders", "Date"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail message fetch failed: {body}");
        }

        #[derive(Deserialize)]
        struct MessageResp {
            id: String,
            #[serde(rename = "threadId")]
            thread_id: String,
            snippet: Option<String>,
            payload: Option<MessagePayload>,
        }

        #[derive(Deserialize)]
        struct MessagePayload {
            #[serde(default)]
            headers: Vec<MessageHeader>,
        }

        #[derive(Deserialize)]
        struct MessageHeader {
            name: String,
            value: String,
        }

        let msg: MessageResp = resp.json().await?;
        let headers = msg.payload.map(|p| p.headers).unwrap_or_default();
        let header_value = |name: &str| {
            headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case(name))
                .map(|h| h.value.clone())
                .unwrap_or_default()
        };

        Ok(GmailMessageSummary {
            id: msg.id,
            thread_id: msg.thread_id,
            from: header_value("From"),
            subject: header_value("Subject"),
            date: header_value("Date"),
            snippet: msg.snippet.unwrap_or_default(),
        })
    }

    pub async fn calendar_list_calendars(
        &self,
        account_id: &str,
    ) -> anyhow::Result<Vec<CalendarListEntry>> {
        let token = self.get_access_token(account_id).await?;
        let mut calendars = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .get("https://www.googleapis.com/calendar/v3/users/me/calendarList")
                .bearer_auth(&token)
                .query(&[("maxResults", "100")]);

            if let Some(ref token) = page_token {
                req = req.query(&[("pageToken", token)]);
            }

            let resp = req.send().await?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Calendar list failed: {body}");
            }

            #[derive(Deserialize)]
            struct CalendarListResp {
                items: Vec<CalendarListEntry>,
                #[serde(rename = "nextPageToken")]
                next_page_token: Option<String>,
            }

            let list: CalendarListResp = resp.json().await?;
            calendars.extend(list.items);

            if let Some(token) = list.next_page_token {
                page_token = Some(token);
            } else {
                break;
            }
        }

        Ok(calendars)
    }

    pub async fn calendar_list_updated_events(
        &self,
        account_id: &str,
        calendar_id: &str,
        updated_min: &str,
    ) -> anyhow::Result<Vec<CalendarEventSummary>> {
        let token = self.get_access_token(account_id).await?;
        let clean_calendar_id = if calendar_id.trim().is_empty() {
            "primary"
        } else {
            calendar_id
        };
        let mut url = reqwest::Url::parse("https://www.googleapis.com/calendar/v3/calendars")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("URL cannot be base"))?
            .push(clean_calendar_id)
            .push("events");

        let resp = self
            .client
            .get(url)
            .bearer_auth(&token)
            .query(&[
                ("maxResults", "50"),
                ("singleEvents", "true"),
                ("orderBy", "updated"),
                ("updatedMin", updated_min),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Calendar updated events failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        let items = data
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut events = Vec::new();
        for item in items {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                continue;
            }
            let summary = item
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled")
                .to_string();
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("confirmed")
                .to_string();
            let updated = item
                .get("updated")
                .and_then(|v| v.as_str())
                .unwrap_or(updated_min)
                .to_string();
            let location = item
                .get("location")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let start = extract_event_time(item.get("start"));
            let end = extract_event_time(item.get("end"));

            events.push(CalendarEventSummary {
                id,
                summary,
                start,
                end,
                status,
                updated,
                location,
            });
        }

        Ok(events)
    }

    async fn gmail_search(
        &self,
        account_id: &str,
        query: &str,
        max_results: Option<u32>,
        page_token: Option<&str>,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let max_results = max_results.unwrap_or(20).to_string();
        let mut req = self
            .client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(&token)
            .query(&[("q", query), ("maxResults", &max_results)]);

        if let Some(token) = page_token {
            req = req.query(&[("pageToken", token)]);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail search failed: {body}");
        }

        #[derive(Deserialize)]
        struct ListResponse {
            #[serde(default)]
            messages: Vec<MessageRef>,
            #[serde(rename = "nextPageToken")]
            next_page_token: Option<String>,
        }
        #[derive(Deserialize, Serialize)]
        struct MessageRef {
            id: String,
            #[serde(rename = "threadId")]
            thread_id: String,
        }

        let list: ListResponse = resp.json().await?;

        if list.messages.is_empty() {
            return Ok(serde_json::to_string_pretty(&serde_json::json!({
                "messages": [],
                "next_page_token": list.next_page_token
            }))?);
        }

        // Fetch snippet for each message
        let mut results = Vec::new();
        for msg_ref in list.messages.iter() {
            let url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
                msg_ref.id
            );
            let resp = self
                .client
                .get(&url)
                .bearer_auth(&token)
                .query(&[
                    ("format", "metadata"),
                    ("metadataHeaders", "From"),
                    ("metadataHeaders", "Subject"),
                    ("metadataHeaders", "Date"),
                ])
                .send()
                .await?;

            if let Ok(msg) = resp.json::<serde_json::Value>().await {
                results.push(serde_json::json!({
                    "id": msg_ref.id,
                    "snippet": msg.get("snippet").and_then(|s| s.as_str()).unwrap_or(""),
                    "headers": msg.pointer("/payload/headers"),
                }));
            }
        }

        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "messages": results,
            "next_page_token": list.next_page_token
        }))?)
    }

    async fn gmail_read(&self, account_id: &str, message_id: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let url = format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}");
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&token)
            .query(&[("format", "full")])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail read failed: {body}");
        }

        let msg: serde_json::Value = resp.json().await?;
        let snippet = msg
            .get("snippet")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        // Extract headers
        let headers = msg
            .pointer("/payload/headers")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();

        let get_header = |name: &str| -> String {
            headers
                .iter()
                .find(|h| h.get("name").and_then(|n| n.as_str()) == Some(name))
                .and_then(|h| h.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        // Extract body - prefer the most informative part (fetch attachment-backed parts if needed)
        let mut body_text = extract_body_from_payload(
            &self.client,
            &token,
            message_id,
            &msg["payload"],
            self.config.debug_gmail_payload,
        )
        .await;
        let mut debug_info = String::new();

        if body_text.trim().is_empty() {
            debug_info = summarize_structure(&msg["payload"], 0);
            tracing::warn!(
                "Empty body for email {}. Structure:\n{}",
                message_id,
                debug_info
            );
            body_text = snippet.clone();
        }

        let attachments = collect_attachments(&msg["payload"]);

        Ok(serde_json::json!({
            "id": message_id,
            "snippet": snippet,
            "from": get_header("From"),
            "to": get_header("To"),
            "subject": get_header("Subject"),
            "date": get_header("Date"),
            "body": body_text,
            "attachments": attachments,
            "debug_structure": if !debug_info.is_empty() { Some(debug_info) } else { None },
        })
        .to_string())
    }

    async fn gmail_send(
        &self,
        account_id: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;

        let raw_email = format!(
            "To: {to}\r\nSubject: {subject}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}"
        );
        use base64::Engine;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw_email.as_bytes());

        let resp = self
            .client
            .post("https://gmail.googleapis.com/gmail/v1/users/me/messages/send")
            .bearer_auth(&token)
            .json(&serde_json::json!({"raw": encoded}))
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail send failed: {body}");
        }

        Ok(format!("Email sent to {to}"))
    }

    async fn drive_search(&self, account_id: &str, query: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let q = format!("name contains '{}'", query.replace('"', "\""));
        let resp = self
            .client
            .get("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(&token)
            .query(&[
                ("q", &q),
                ("pageSize", &"20".to_string()),
                (
                    "fields",
                    &"files(id,name,mimeType,modifiedTime)".to_string(),
                ),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Drive search failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(serde_json::to_string_pretty(&data["files"])?)
    }

    async fn drive_read(&self, account_id: &str, file_id: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;

        // First get file metadata to check mime type
        let meta_url = format!("https://www.googleapis.com/drive/v3/files/{file_id}");
        let meta_resp = self
            .client
            .get(&meta_url)
            .bearer_auth(&token)
            .query(&[("fields", "id,name,mimeType,size")])
            .send()
            .await?;

        let meta: serde_json::Value = meta_resp.json().await?;
        let mime = meta.get("mimeType").and_then(|m| m.as_str()).unwrap_or("");

        // For Google Docs, export as plain text
        let content = if mime.starts_with("application/vnd.google-apps.") {
            let export_mime = match mime {
                "application/vnd.google-apps.document" => "text/plain",
                "application/vnd.google-apps.spreadsheet" => "text/csv",
                "application/vnd.google-apps.presentation" => "text/plain",
                _ => "text/plain",
            };
            let export_url = format!("https://www.googleapis.com/drive/v3/files/{file_id}/export");
            let resp = self
                .client
                .get(&export_url)
                .bearer_auth(&token)
                .query(&[("mimeType", export_mime)])
                .send()
                .await?;
            resp.text().await?
        } else {
            // For binary files, just return metadata
            return Ok(serde_json::json!({
                "id": file_id,
                "name": meta.get("name"),
                "mimeType": mime,
                "note": "Binary file - download not supported via chat. Use Drive UI."
            })
            .to_string());
        };

        Ok(serde_json::json!({
            "id": file_id,
            "name": meta.get("name"),
            "mimeType": mime,
            "content": content,
        })
        .to_string())
    }

    async fn drive_list_files(
        &self,
        account_id: &str,
        folder_id: Option<&str>,
        query: Option<&str>,
        page_size: Option<u32>,
        page_token: Option<&str>,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let page_size = page_size.unwrap_or(20).min(1000).to_string();

        // Build the query string
        let mut q_parts = Vec::new();

        // If folder_id is specified, filter by parent
        if let Some(fid) = folder_id {
            if !fid.trim().is_empty() {
                q_parts.push(format!("'{}' in parents", fid.replace("'", "\\'").trim()));
            }
        }

        // Add trashed filter
        q_parts.push("trashed = false".to_string());

        // If query is specified, add name search
        if let Some(q) = query {
            if !q.trim().is_empty() {
                q_parts.push(format!("name contains '{}'", q.replace("'", "\\'").trim()));
            }
        }

        let full_query = q_parts.join(" and ");

        let mut req = self
            .client
            .get("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(&token)
            .query(&[
                ("q", &full_query),
                ("pageSize", &page_size),
                (
                    "fields",
                    &"nextPageToken,files(id,name,mimeType,size,modifiedTime,webViewLink,parents)"
                        .to_string(),
                ),
                ("orderBy", &"folder,name".to_string()),
            ]);

        if let Some(token) = page_token {
            req = req.query(&[("pageToken", token)]);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Drive list files failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(serde_json::to_string_pretty(&data)?)
    }

    async fn calendar_list_events(
        &self,
        account_id: &str,
        calendar_id: Option<String>,
        query: Option<String>,
        time_min: Option<String>,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let calendar_id = calendar_id.unwrap_or_else(|| "primary".to_string());
        let mut url = reqwest::Url::parse("https://www.googleapis.com/calendar/v3/calendars")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("URL cannot be base"))?
            .push(&calendar_id)
            .push("events");

        let mut req = self.client.get(url).bearer_auth(&token).query(&[
            ("maxResults", "10"),
            ("singleEvents", "true"),
            ("orderBy", "startTime"),
        ]);

        if let Some(q) = query {
            req = req.query(&[("q", q)]);
        }

        let tm = time_min.unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        req = req.query(&[("timeMin", tm)]);

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Calendar list failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        let events = data.get("items").unwrap_or(&serde_json::json!([])).clone();

        Ok(serde_json::to_string_pretty(&events)?)
    }

    async fn calendar_create_event(
        &self,
        account_id: &str,
        calendar_id: Option<String>,
        summary: &str,
        start_time: &str,
        end_time: &str,
        description: Option<String>,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let calendar_id = calendar_id.unwrap_or_else(|| "primary".to_string());
        let mut url = reqwest::Url::parse("https://www.googleapis.com/calendar/v3/calendars")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("URL cannot be base"))?
            .push(&calendar_id)
            .push("events");

        let body = serde_json::json!({
            "summary": summary,
            "description": description.unwrap_or_default(),
            "start": { "dateTime": start_time },
            "end": { "dateTime": end_time }
        });

        let resp = self
            .client
            .post(url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Calendar create failed: {body}");
        }

        let event: serde_json::Value = resp.json().await?;
        Ok(serde_json::to_string_pretty(&event)?)
    }

    async fn poll_gmail_for_account(
        &self,
        db: &Arc<Database>,
        acc: &IntegrationAccount,
    ) -> anyhow::Result<()> {
        let history_key = format!("gmail_history_id:{}", acc.id);
        let history_id = match db.get_integration_setting("google", &history_key).await? {
            Some(val) => val,
            None => {
                let profile = self.gmail_get_profile(&acc.id).await?;
                db.set_integration_setting("google", &history_key, &profile.history_id)
                    .await?;
                return Ok(());
            }
        };

        match self.gmail_list_history(&acc.id, &history_id).await? {
            GmailHistoryOutcome::Reset { history_id } => {
                db.set_integration_setting("google", &history_key, &history_id)
                    .await?;
                return Ok(());
            }
            GmailHistoryOutcome::Updated(result) => {
                let account_email = self.get_account_email(acc);
                for msg in result.messages {
                    tracing::info!("New Gmail message: {}", msg.id);
                    let payload = serde_json::json!({
                        "message_id": msg.id,
                        "thread_id": msg.thread_id,
                        "from": msg.from,
                        "subject": msg.subject,
                        "date": msg.date,
                        "snippet": msg.snippet,
                        "account_id": acc.id,
                        "account_email": account_email,
                    });
                    let _ = db
                        .insert_integration_event(
                            "google",
                            &acc.id,
                            "gmail_new_message",
                            &msg.id,
                            &payload,
                        )
                        .await?;
                }
                db.set_integration_setting("google", &history_key, &result.history_id)
                    .await?;
            }
        }

        Ok(())
    }

    async fn poll_calendar_for_account(
        &self,
        db: &Arc<Database>,
        acc: &IntegrationAccount,
    ) -> anyhow::Result<()> {
        let calendars = match self.calendar_list_calendars(&acc.id).await {
            Ok(cals) => cals,
            Err(e) => {
                tracing::error!("Failed to list calendars for account {}: {}", acc.id, e);
                return Err(e);
            }
        };

        let account_email = self.get_account_email(acc);

        for calendar in calendars {
            let calendar_id = &calendar.id;
            let updated_key = format!("calendar_updated_min:{}:{}", acc.id, calendar_id);

            // Handle legacy key "calendar_updated_min:{acc.id}" for primary calendar
            let db_key = if calendar.primary {
                updated_key.clone()
            } else {
                updated_key.clone()
            };

            let updated_min = match db.get_integration_setting("google", &db_key).await? {
                Some(val) => val,
                None => {
                    // If this is primary, check if we have the old legacy key
                    if calendar.primary {
                        if let Some(val) = db
                            .get_integration_setting(
                                "google",
                                &format!("calendar_updated_min:{}", acc.id),
                            )
                            .await?
                        {
                            val
                        } else {
                            // Default to now
                            let now = Utc::now().to_rfc3339();
                            db.set_integration_setting("google", &db_key, &now).await?;
                            now
                        }
                    } else {
                        let now = Utc::now().to_rfc3339();
                        db.set_integration_setting("google", &db_key, &now).await?;
                        now
                    }
                }
            };

            match self
                .calendar_list_updated_events(&acc.id, calendar_id, &updated_min)
                .await
            {
                Ok(events) => {
                    let mut max_updated = updated_min.clone();
                    for ev in events {
                        if ev.updated > max_updated {
                            max_updated = ev.updated.clone();
                        }
                        let dedupe_key = format!("{}:{}:{}", calendar_id, ev.id, ev.updated);
                        let payload = serde_json::json!({
                            "event_id": ev.id,
                            "calendar_id": calendar_id,
                            "calendar_summary": calendar.summary,
                            "summary": ev.summary,
                            "start": ev.start,
                            "end": ev.end,
                            "status": ev.status,
                            "updated": ev.updated,
                            "location": ev.location,
                            "account_id": acc.id,
                            "account_email": account_email,
                        });
                        let _ = db
                            .insert_integration_event(
                                "google",
                                &acc.id,
                                "calendar_event_updated",
                                &dedupe_key,
                                &payload,
                            )
                            .await?;
                    }

                    if max_updated != updated_min {
                        db.set_integration_setting("google", &db_key, &max_updated)
                            .await?;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to poll calendar {} for account {}: {}",
                        calendar_id,
                        acc.id,
                        e
                    );
                }
            }
        }

        Ok(())
    }

    fn get_account_email(&self, acc: &IntegrationAccount) -> Option<String> {
        serde_json::from_str::<StoredAccount>(&acc.data)
            .ok()
            .map(|data| data.email)
    }
}

fn summarize_structure(payload: &serde_json::Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let mime = payload
        .get("mimeType")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown");
    let has_data = payload.pointer("/body/data").is_some();
    let att_id = payload.pointer("/body/attachmentId").is_some();
    let size = payload
        .pointer("/body/size")
        .and_then(|s| s.as_u64())
        .unwrap_or(0);

    let mut out = format!(
        "{}Mime: {}, size: {}, has_data: {}, has_att_id: {}\n",
        indent, mime, size, has_data, att_id
    );

    if let Some(parts) = payload.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            out.push_str(&summarize_structure(part, depth + 1));
        }
    }
    out
}

fn decode_base64_url(data: &str) -> Option<String> {
    use base64::Engine;

    // Gmail sometimes includes line breaks or padding; normalize before decode.
    let cleaned: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();

    let try_decode = |engine: base64::engine::general_purpose::GeneralPurpose| {
        engine.decode(cleaned.as_bytes()).ok()
    };

    try_decode(base64::engine::general_purpose::URL_SAFE_NO_PAD)
        .or_else(|| try_decode(base64::engine::general_purpose::URL_SAFE))
        .or_else(|| try_decode(base64::engine::general_purpose::STANDARD_NO_PAD))
        .or_else(|| try_decode(base64::engine::general_purpose::STANDARD))
        .map(|decoded| String::from_utf8_lossy(&decoded).to_string())
}

async fn extract_body_from_payload(
    client: &reqwest::Client,
    token: &str,
    message_id: &str,
    payload: &serde_json::Value,
    debug: bool,
) -> String {
    let text = extract_content(client, token, message_id, payload, "text/plain", debug)
        .await
        .unwrap_or_default();
    let html = extract_content(client, token, message_id, payload, "text/html", debug)
        .await
        .unwrap_or_default();

    choose_preferred_body(text, html)
}

fn choose_preferred_body(text: String, html: String) -> String {
    let text_trimmed = text.trim();
    let html_trimmed = html.trim();
    if text_trimmed.is_empty() && !html_trimmed.is_empty() {
        return html;
    }
    if html_trimmed.is_empty() {
        return text;
    }

    let text_len = text_trimmed.len();
    let html_visible_len = approx_visible_len(html_trimmed);

    // Prefer HTML if it contains substantially more visible content.
    if html_visible_len > text_len.saturating_mul(2)
        || (text_len < 200 && html_visible_len > text_len)
    {
        return html;
    }

    text
}

fn approx_visible_len(html: &str) -> usize {
    let mut in_tag = false;
    let mut count = 0usize;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {
                if !in_tag && !ch.is_whitespace() {
                    count += 1;
                }
            }
        }
    }
    count
}

fn collect_attachments(payload: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut stack = vec![payload];

    while let Some(part) = stack.pop() {
        if let Some(parts) = part.get("parts").and_then(|p| p.as_array()) {
            for child in parts.iter().rev() {
                stack.push(child);
            }
        }

        let filename = part.get("filename").and_then(|f| f.as_str()).unwrap_or("");
        let mime = part.get("mimeType").and_then(|m| m.as_str()).unwrap_or("");
        let attachment_id = part.pointer("/body/attachmentId").and_then(|a| a.as_str());
        let size = part
            .pointer("/body/size")
            .and_then(|s| s.as_u64())
            .unwrap_or(0);

        let is_non_text = !mime.to_lowercase().starts_with("text/");
        let has_payload = attachment_id.is_some() || size > 0;

        if (is_non_text && has_payload) || (!filename.is_empty() && has_payload) {
            out.push(serde_json::json!({
                "filename": if filename.is_empty() { None::<String> } else { Some(filename.to_string()) },
                "mimeType": if mime.is_empty() { None::<String> } else { Some(mime.to_string()) },
                "size": size,
                "attachmentId": attachment_id.map(|a| a.to_string()),
            }));
        }
    }

    out
}

async fn extract_content(
    client: &reqwest::Client,
    token: &str,
    message_id: &str,
    payload: &serde_json::Value,
    target_mime: &str,
    debug: bool,
) -> Option<String> {
    let mut stack = vec![payload];
    while let Some(part) = stack.pop() {
        if let Some(mime) = part.get("mimeType").and_then(|m| m.as_str()) {
            if mime.to_lowercase().starts_with(target_mime) {
                if let Some(data) = part.pointer("/body/data").and_then(|d| d.as_str()) {
                    if let Some(decoded) = decode_base64_url(data) {
                        return Some(decoded);
                    }
                    log_decode_failure("body.data", message_id, mime, data, debug);
                }
                if let Some(att_id) = part.pointer("/body/attachmentId").and_then(|a| a.as_str()) {
                    if let Some(decoded) =
                        fetch_attachment_text(client, token, message_id, att_id, mime, debug).await
                    {
                        return Some(decoded);
                    }
                }
            }
        }
        if let Some(parts) = part.get("parts").and_then(|p| p.as_array()) {
            for child in parts.iter().rev() {
                stack.push(child);
            }
        }
    }
    None
}

async fn fetch_attachment_text(
    client: &reqwest::Client,
    token: &str,
    message_id: &str,
    attachment_id: &str,
    mime: &str,
    debug: bool,
) -> Option<String> {
    if attachment_id.trim().is_empty() {
        return None;
    }

    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}/attachments/{attachment_id}"
    );
    let resp = match client.get(&url).bearer_auth(token).send().await {
        Ok(resp) => resp,
        Err(err) => {
            tracing::warn!("Gmail attachment fetch failed for {}: {}", message_id, err);
            return None;
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            "Gmail attachment fetch failed for {} ({}): {}",
            message_id,
            status,
            body
        );
        return None;
    }

    let data: serde_json::Value = match resp.json().await {
        Ok(json) => json,
        Err(err) => {
            tracing::warn!(
                "Gmail attachment JSON parse failed for {}: {}",
                message_id,
                err
            );
            return None;
        }
    };

    let Some(raw) = data.get("data").and_then(|d| d.as_str()) else {
        return None;
    };
    let decoded = decode_base64_url(raw);
    if decoded.is_none() {
        log_decode_failure("attachment.data", message_id, mime, raw, debug);
    }
    decoded
}

fn log_decode_failure(source: &str, message_id: &str, mime: &str, raw: &str, debug: bool) {
    if !debug {
        return;
    }

    let mut whitespace = 0usize;
    let mut invalid = 0usize;
    let mut url_safe = 0usize;
    let mut cleaned_len = 0usize;
    let mut prefix = String::new();

    for ch in raw.chars() {
        if ch.is_ascii_whitespace() {
            whitespace += 1;
            continue;
        }
        cleaned_len += 1;
        if prefix.len() < 12 {
            prefix.push(ch);
        }
        let valid = ch.is_ascii_alphanumeric()
            || ch == '+'
            || ch == '/'
            || ch == '='
            || ch == '-'
            || ch == '_';
        if !valid {
            invalid += 1;
        }
        if ch == '-' || ch == '_' {
            url_safe += 1;
        }
    }

    let has_padding = raw.contains('=');
    let mime = if mime.is_empty() { "unknown" } else { mime };

    tracing::warn!(
        "Gmail base64 decode failed ({source}) msg={} mime={} len={} whitespace={} invalid={} urlsafe={} padding={} prefix={}",
        message_id,
        mime,
        cleaned_len,
        whitespace,
        invalid,
        url_safe,
        has_padding,
        prefix
    );
}

fn extract_event_time(value: Option<&serde_json::Value>) -> Option<String> {
    let v = value?;
    if let Some(dt) = v.get("dateTime").and_then(|x| x.as_str()) {
        return Some(dt.to_string());
    }
    if let Some(date) = v.get("date").and_then(|x| x.as_str()) {
        return Some(date.to_string());
    }
    None
}

#[async_trait::async_trait]
impl Integration for GoogleIntegration {
    fn name(&self) -> &str {
        "google"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "google_list_accounts".to_string(),
                description: "List configured Google accounts".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "gmail_search".to_string(),
                description: "Search Gmail messages with pagination support".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "query": {"type": "string", "description": "Gmail search query (same syntax as Gmail search bar)"},
                        "max_results": {"type": ["integer", "null"], "description": "Maximum number of messages to return (default: 20, max: 500)"},
                        "page_token": {"type": ["string", "null"], "description": "Token for pagination from previous search results"}
                    },
                    "required": ["account_id", "query", "max_results", "page_token"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "gmail_read".to_string(),
                description: "Read a specific Gmail message by ID".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "message_id": {"type": "string", "description": "Gmail message ID from search results"}
                    },
                    "required": ["account_id", "message_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "gmail_send".to_string(),
                description: "Send an email via Gmail".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "to": {"type": "string", "description": "Recipient email address"},
                        "subject": {"type": "string", "description": "Email subject"},
                        "body": {"type": "string", "description": "Email body text"}
                    },
                    "required": ["account_id", "to", "subject", "body"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "drive_search".to_string(),
                description: "Search Google Drive files by name".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "query": {"type": "string", "description": "Search term for file names"}
                    },
                    "required": ["account_id", "query"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "drive_read".to_string(),
                description: "Read content of a Google Drive file".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "file_id": {"type": "string", "description": "Google Drive file ID from search results"}
                    },
                    "required": ["account_id", "file_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "drive_list_files".to_string(),
                description:
                    "List Google Drive files with optional folder filtering and pagination"
                        .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "folder_id": {"type": ["string", "null"], "description": "Optional folder ID to list files within (null for root or all files)"},
                        "query": {"type": ["string", "null"], "description": "Optional search query to filter files by name"},
                        "page_size": {"type": ["integer", "null"], "description": "Number of files to return (default: 20, max: 1000)"},
                        "page_token": {"type": ["string", "null"], "description": "Token for pagination from previous results"}
                    },
                    "required": ["account_id", "folder_id", "query", "page_size", "page_token"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "calendar_list_calendars".to_string(),
                description: "List all Google calendars accessible by the user".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"}
                    },
                    "required": ["account_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "calendar_list_events".to_string(),
                description: "List upcoming Google Calendar events".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "calendar_id": {"type": ["string", "null"], "description": "Calendar ID (optional, defaults to primary)"},
                        "query": {"type": "string", "description": "Filter events by text query (use empty string for none)"},
                        "time_min": {"type": "string", "description": "Start time (ISO 8601) to list events from (use empty string for now)"}
                    },
                    "required": ["account_id", "query", "time_min", "calendar_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "calendar_create_event".to_string(),
                description: "Create a new Google Calendar event".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "calendar_id": {"type": ["string", "null"], "description": "Calendar ID (optional, defaults to primary)"},
                        "summary": {"type": "string", "description": "Event title"},
                        "start_time": {"type": "string", "description": "Start time (ISO 8601)"},
                        "end_time": {"type": "string", "description": "End time (ISO 8601)"},
                        "description": {"type": "string", "description": "Event description (use empty string for none)"}
                    },
                    "required": ["account_id", "summary", "start_time", "end_time", "description", "calendar_id"],
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        tracing::debug!("google.execute: {tool_name}");

        if tool_name == "google_list_accounts" {
            return self.list_accounts().await;
        }

        match tool_name {
            "gmail_search" => {
                #[derive(Deserialize)]
                struct Args {
                    query: String,
                    account_id: String,
                    max_results: Option<u32>,
                    page_token: Option<String>,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.gmail_search(
                    &args.account_id,
                    &args.query,
                    args.max_results,
                    args.page_token.as_deref(),
                )
                .await
            }
            "gmail_read" => {
                #[derive(Deserialize)]
                struct Args {
                    message_id: String,
                    account_id: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.gmail_read(&args.account_id, &args.message_id).await
            }
            "gmail_send" => {
                #[derive(Deserialize)]
                struct Args {
                    to: String,
                    subject: String,
                    body: String,
                    account_id: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.gmail_send(&args.account_id, &args.to, &args.subject, &args.body)
                    .await
            }
            "drive_search" => {
                #[derive(Deserialize)]
                struct Args {
                    query: String,
                    account_id: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.drive_search(&args.account_id, &args.query).await
            }
            "drive_read" => {
                #[derive(Deserialize)]
                struct Args {
                    file_id: String,
                    account_id: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.drive_read(&args.account_id, &args.file_id).await
            }
            "drive_list_files" => {
                #[derive(Deserialize)]
                struct Args {
                    account_id: String,
                    folder_id: Option<String>,
                    query: Option<String>,
                    page_size: Option<u32>,
                    page_token: Option<String>,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.drive_list_files(
                    &args.account_id,
                    args.folder_id.as_deref(),
                    args.query.as_deref(),
                    args.page_size,
                    args.page_token.as_deref(),
                )
                .await
            }
            "calendar_list_calendars" => {
                #[derive(Deserialize)]
                struct Args {
                    account_id: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let cals = self.calendar_list_calendars(&args.account_id).await?;
                Ok(serde_json::to_string_pretty(&cals)?)
            }
            "calendar_list_events" => {
                #[derive(Deserialize)]
                struct Args {
                    query: String,
                    time_min: String,
                    account_id: String,
                    calendar_id: Option<String>,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let query = args.query.trim();
                let time_min = args.time_min.trim();
                let query = if query.is_empty() {
                    None
                } else {
                    Some(query.to_string())
                };
                let time_min = if time_min.is_empty() {
                    None
                } else {
                    Some(time_min.to_string())
                };
                let calendar_id = args.calendar_id.filter(|c| !c.trim().is_empty());
                self.calendar_list_events(&args.account_id, calendar_id, query, time_min)
                    .await
            }
            "calendar_create_event" => {
                #[derive(Deserialize)]
                struct Args {
                    summary: String,
                    start_time: String,
                    end_time: String,
                    description: String,
                    account_id: String,
                    calendar_id: Option<String>,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let description = if args.description.trim().is_empty() {
                    None
                } else {
                    Some(args.description)
                };
                let calendar_id = args.calendar_id.filter(|c| !c.trim().is_empty());
                self.calendar_create_event(
                    &args.account_id,
                    calendar_id,
                    &args.summary,
                    &args.start_time,
                    &args.end_time,
                    description,
                )
                .await
            }
            _ => anyhow::bail!("Unknown google tool: {tool_name}"),
        }
    }

    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        if let Some(db) = &self.db {
            let accounts = db.list_integration_accounts("google").await?;
            if !accounts.is_empty() {
                return Ok(OnboardingStatus::Configured);
            }
        }

        let redirect_uri = "http://localhost:3000/oauth/callback";
        let url = Self::generate_auth_url(&self.config, redirect_uri, None);

        Ok(OnboardingStatus::RequiresAction {
            fields: vec![OnboardingField {
                name: "refresh_token".to_string(),
                label: "Connect Google Account".to_string(),
                input_type: "oauth".to_string(),
                value: Some(url),
                description: Some("Click to authorize Jossie with Google".to_string()),
            }],
        })
    }

    async fn poll(&self) -> anyhow::Result<()> {
        let Some(db) = &self.db else {
            return Ok(());
        };

        let accounts = db.list_integration_accounts("google").await?;
        for acc in accounts {
            if self.is_account_paused(db, &acc).await? {
                tracing::warn!("Skipping paused Google account {} during poll", acc.id);
                if let Err(e) = self.queue_reconnect_notice_if_due(db, &acc).await {
                    tracing::warn!(
                        "Failed to queue reconnect reminder for paused Google account {}: {e}",
                        acc.id
                    );
                }
                continue;
            }

            if let Err(e) = self.poll_gmail_for_account(db, &acc).await {
                if Self::is_invalid_grant_text(&e.to_string()) {
                    tracing::warn!(
                        "Pausing Google account {} due to invalid_grant token refresh failure",
                        acc.id
                    );
                    self.pause_account_invalid_grant(db, &acc, &e.to_string())
                        .await?;
                    if let Err(notice_err) = self.queue_reconnect_notice_if_due(db, &acc).await {
                        tracing::warn!(
                            "Failed to queue reconnect reminder for account {}: {notice_err}",
                            acc.id
                        );
                    }
                    continue;
                }
                tracing::warn!("Gmail poll failed for account {}: {e}", acc.id);
            }
            if let Err(e) = self.poll_calendar_for_account(db, &acc).await {
                if Self::is_invalid_grant_text(&e.to_string()) {
                    tracing::warn!(
                        "Pausing Google account {} due to invalid_grant token refresh failure",
                        acc.id
                    );
                    self.pause_account_invalid_grant(db, &acc, &e.to_string())
                        .await?;
                    if let Err(notice_err) = self.queue_reconnect_notice_if_due(db, &acc).await {
                        tracing::warn!(
                            "Failed to queue reconnect reminder for account {}: {notice_err}",
                            acc.id
                        );
                    }
                    continue;
                }
                tracing::warn!("Calendar poll failed for account {}: {e}", acc.id);
            }
        }

        Ok(())
    }
}
