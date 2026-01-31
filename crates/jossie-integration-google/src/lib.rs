use jossie_core::config::GoogleConfig;
use jossie_core::integration::{Integration, ToolDefinition, OnboardingStatus, OnboardingField};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use jossie_db::Database;
use std::collections::HashMap;

pub struct GoogleIntegration {
    config: GoogleConfig,
    client: reqwest::Client,
    tokens: Arc<RwLock<HashMap<String, TokenData>>>,
    db: Option<Arc<Database>>,
}

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

    pub fn generate_auth_url(config: &GoogleConfig, redirect_uri: &str) -> String {
        let scopes = [
            "https://mail.google.com/",
            "https://www.googleapis.com/auth/drive",
            "https://www.googleapis.com/auth/gmail.send",
            "https://www.googleapis.com/auth/calendar"
        ].join(" ");

        format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
            config.client_id,
            redirect_uri,
            scopes
        )
    }

    pub async fn exchange_code(config: &GoogleConfig, code: &str, redirect_uri: &str) -> anyhow::Result<String> {
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

    async fn get_refresh_token(&self, account_id: Option<&str>) -> anyhow::Result<String> {
        let account_id = account_id.unwrap_or("default");

        if account_id == "default" {
            if !self.config.refresh_token.is_empty() {
                return Ok(self.config.refresh_token.clone());
            }
            if let Some(db) = &self.db {
                if let Ok(Some(val)) = db.get_integration_setting("google", "refresh_token").await {
                    return Ok(val);
                }
            }
            anyhow::bail!("No default Google account configured");
        }

        // Look up in DB
        if let Some(db) = &self.db {
            if let Some(acc) = db.get_integration_account(account_id).await? {
                let stored: StoredAccount = serde_json::from_str(&acc.data)?;
                return Ok(stored.refresh_token);
            }
        }

        anyhow::bail!("Account not found: {}", account_id)
    }

    async fn get_access_token(&self, account_id: Option<&str>) -> anyhow::Result<String> {
        let account_key = account_id.unwrap_or("default").to_string();

        // Check cached token
        {
            let tokens = self.tokens.read().await;
            if let Some(td) = tokens.get(&account_key) {
                if td.expires_at > std::time::Instant::now() {
                    return Ok(td.access_token.clone());
                }
            }
        }

        let refresh_token = self.get_refresh_token(Some(&account_key)).await?;

        // Refresh token
        let resp = self.client
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
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(tr.expires_in.saturating_sub(60)),
        };

        self.tokens.write().await.insert(account_key, td.clone());
        Ok(td.access_token)
    }

    async fn list_accounts(&self) -> anyhow::Result<String> {
        let mut accounts = Vec::new();
        
        // Default account
        let has_default = !self.config.refresh_token.is_empty() || 
            (self.db.is_some() && self.db.as_ref().unwrap().get_integration_setting("google", "refresh_token").await.unwrap_or(None).is_some());
            
        if has_default {
            accounts.push(serde_json::json!({
                "id": "default",
                "name": "Default Account",
                "type": "config/legacy"
            }));
        }

        if let Some(db) = &self.db {
            let db_accounts = db.list_integration_accounts("google").await?;
            for acc in db_accounts {
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

    async fn gmail_search(&self, account_id: Option<&str>, query: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let resp = self.client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(&token)
            .query(&[("q", query), ("maxResults", "20")])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail search failed: {body}");
        }

        #[derive(Deserialize)]
        struct ListResponse {
            #[serde(default)]
            messages: Vec<MessageRef>,
        }
        #[derive(Deserialize, Serialize)]
        struct MessageRef {
            id: String,
            #[serde(rename = "threadId")]
            thread_id: String,
        }

        let list: ListResponse = resp.json().await?;

        if list.messages.is_empty() {
            return Ok("No matching emails found.".to_string());
        }

        // Fetch snippet for each message (up to 10)
        let mut results = Vec::new();
        for msg_ref in list.messages.iter().take(10) {
            let url = format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{}", msg_ref.id);
            let resp = self.client
                .get(&url)
                .bearer_auth(&token)
                .query(&[("format", "metadata"), ("metadataHeaders", "From"), ("metadataHeaders", "Subject"), ("metadataHeaders", "Date")])
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

        Ok(serde_json::to_string_pretty(&results)?)
    }

    async fn gmail_read(&self, account_id: Option<&str>, message_id: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let url = format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}");
        let resp = self.client
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
        let snippet = msg.get("snippet").and_then(|s| s.as_str()).unwrap_or("").to_string();

        // Extract headers
        let headers = msg.pointer("/payload/headers")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();

        let get_header = |name: &str| -> String {
            headers.iter()
                .find(|h| h.get("name").and_then(|n| n.as_str()) == Some(name))
                .and_then(|h| h.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        // Extract body - try plain text first, then html
        let mut body_text = extract_body_from_payload(&msg["payload"]);
        let mut debug_info = String::new();

        if body_text.trim().is_empty() {
            debug_info = summarize_structure(&msg["payload"], 0);
            tracing::warn!("Empty body for email {}. Structure:\n{}", message_id, debug_info);
            body_text = snippet.clone();
        }

        Ok(serde_json::json!({
            "id": message_id,
            "snippet": snippet,
            "from": get_header("From"),
            "to": get_header("To"),
            "subject": get_header("Subject"),
            "date": get_header("Date"),
            "body": body_text,
            "debug_structure": if !debug_info.is_empty() { Some(debug_info) } else { None },
        }).to_string())
    }

    async fn gmail_send(&self, account_id: Option<&str>, to: &str, subject: &str, body: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;

        let raw_email = format!(
            "To: {to}\r\nSubject: {subject}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}"
        );
        use base64::Engine;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw_email.as_bytes());

        let resp = self.client
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

    async fn drive_search(&self, account_id: Option<&str>, query: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let q = format!("name contains '{}'", query.replace('"', "\""));
        let resp = self.client
            .get("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(&token)
            .query(&[("q", &q), ("pageSize", &"20".to_string()), ("fields", &"files(id,name,mimeType,modifiedTime)".to_string())])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Drive search failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(serde_json::to_string_pretty(&data["files"])?)
    }

    async fn drive_read(&self, account_id: Option<&str>, file_id: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;

        // First get file metadata to check mime type
        let meta_url = format!("https://www.googleapis.com/drive/v3/files/{file_id}");
        let meta_resp = self.client
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
            let resp = self.client
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
            }).to_string());
        };

        Ok(serde_json::json!({
            "id": file_id,
            "name": meta.get("name"),
            "mimeType": mime,
            "content": content,
        }).to_string())
    }

    async fn calendar_list_events(&self, account_id: Option<&str>, query: Option<String>, time_min: Option<String>) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let mut req = self.client
            .get("https://www.googleapis.com/calendar/v3/calendars/primary/events")
            .bearer_auth(&token)
            .query(&[("maxResults", "10"), ("singleEvents", "true"), ("orderBy", "startTime")]);
        
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

    async fn calendar_create_event(&self, account_id: Option<&str>, summary: &str, start_time: &str, end_time: &str, description: Option<String>) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        
        let body = serde_json::json!({
            "summary": summary,
            "description": description.unwrap_or_default(),
            "start": { "dateTime": start_time },
            "end": { "dateTime": end_time }
        });

        let resp = self.client
            .post("https://www.googleapis.com/calendar/v3/calendars/primary/events")
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
}

fn summarize_structure(payload: &serde_json::Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let mime = payload.get("mimeType").and_then(|m| m.as_str()).unwrap_or("unknown");
    let has_data = payload.pointer("/body/data").is_some();
    let att_id = payload.pointer("/body/attachmentId").is_some();
    let size = payload.pointer("/body/size").and_then(|s| s.as_u64()).unwrap_or(0);
    
    let mut out = format!("{}Mime: {}, size: {}, has_data: {}, has_att_id: {}\n", indent, mime, size, has_data, att_id);
    
    if let Some(parts) = payload.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            out.push_str(&summarize_structure(part, depth + 1));
        }
    }
    out
}

fn extract_body_from_payload(payload: &serde_json::Value) -> String {
    let text = extract_content(payload, "text/plain").unwrap_or_default();
    let html = extract_content(payload, "text/html").unwrap_or_default();
    
    if text.trim().is_empty() && !html.trim().is_empty() {
        return html;
    }
    if !text.is_empty() {
        return text;
    }
    String::new()
}

fn extract_content(payload: &serde_json::Value, target_mime: &str) -> Option<String> {
    if let Some(mime) = payload.get("mimeType").and_then(|m| m.as_str()) {
        if mime.to_lowercase().starts_with(target_mime) {
            if let Some(data) = payload.pointer("/body/data").and_then(|d| d.as_str()) {
                use base64::Engine;
                if let Ok(decoded) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data) {
                    return Some(String::from_utf8_lossy(&decoded).to_string());
                }
            }
        }
    }
    if let Some(parts) = payload.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            if let Some(content) = extract_content(part, target_mime) {
                return Some(content);
            }
        }
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
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
            ToolDefinition {
                name: "gmail_search".to_string(),
                description: "Search Gmail messages".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID (optional, defaults to default account)"},
                        "query": {"type": "string", "description": "Gmail search query (same syntax as Gmail search bar)"}
                    },
                    "required": ["query"]
                }),
            },
            ToolDefinition {
                name: "gmail_read".to_string(),
                description: "Read a specific Gmail message by ID".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID"},
                        "message_id": {"type": "string", "description": "Gmail message ID from search results"}
                    },
                    "required": ["message_id"]
                }),
            },
            ToolDefinition {
                name: "gmail_send".to_string(),
                description: "Send an email via Gmail".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID"},
                        "to": {"type": "string", "description": "Recipient email address"},
                        "subject": {"type": "string", "description": "Email subject"},
                        "body": {"type": "string", "description": "Email body text"}
                    },
                    "required": ["to", "subject", "body"]
                }),
            },
            ToolDefinition {
                name: "drive_search".to_string(),
                description: "Search Google Drive files by name".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID"},
                        "query": {"type": "string", "description": "Search term for file names"}
                    },
                    "required": ["query"]
                }),
            },
            ToolDefinition {
                name: "drive_read".to_string(),
                description: "Read content of a Google Drive file".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID"},
                        "file_id": {"type": "string", "description": "Google Drive file ID from search results"}
                    },
                    "required": ["file_id"]
                }),
            },
            ToolDefinition {
                name: "calendar_list_events".to_string(),
                description: "List upcoming Google Calendar events".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID"},
                        "query": {"type": "string", "description": "Filter events by text query"},
                        "time_min": {"type": "string", "description": "Start time (ISO 8601) to list events from. Defaults to now."}
                    }
                }),
            },
            ToolDefinition {
                name: "calendar_create_event".to_string(),
                description: "Create a new Google Calendar event".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID"},
                        "summary": {"type": "string", "description": "Event title"},
                        "start_time": {"type": "string", "description": "Start time (ISO 8601)"},
                        "end_time": {"type": "string", "description": "End time (ISO 8601)"},
                        "description": {"type": "string", "description": "Event description (optional)"}
                    },
                    "required": ["summary", "start_time", "end_time"]
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
                struct Args { query: String, account_id: Option<String> }
                let args: Args = serde_json::from_str(arguments)?;
                self.gmail_search(args.account_id.as_deref(), &args.query).await
            }
            "gmail_read" => {
                #[derive(Deserialize)]
                struct Args { message_id: String, account_id: Option<String> }
                let args: Args = serde_json::from_str(arguments)?;
                self.gmail_read(args.account_id.as_deref(), &args.message_id).await
            }
            "gmail_send" => {
                #[derive(Deserialize)]
                struct Args { to: String, subject: String, body: String, account_id: Option<String> }
                let args: Args = serde_json::from_str(arguments)?;
                self.gmail_send(args.account_id.as_deref(), &args.to, &args.subject, &args.body).await
            }
            "drive_search" => {
                #[derive(Deserialize)]
                struct Args { query: String, account_id: Option<String> }
                let args: Args = serde_json::from_str(arguments)?;
                self.drive_search(args.account_id.as_deref(), &args.query).await
            }
            "drive_read" => {
                #[derive(Deserialize)]
                struct Args { file_id: String, account_id: Option<String> }
                let args: Args = serde_json::from_str(arguments)?;
                self.drive_read(args.account_id.as_deref(), &args.file_id).await
            }
            "calendar_list_events" => {
                #[derive(Deserialize)]
                struct Args { query: Option<String>, time_min: Option<String>, account_id: Option<String> }
                let args: Args = serde_json::from_str(arguments)?;
                self.calendar_list_events(args.account_id.as_deref(), args.query, args.time_min).await
            }
            "calendar_create_event" => {
                #[derive(Deserialize)]
                struct Args { summary: String, start_time: String, end_time: String, description: Option<String>, account_id: Option<String> }
                let args: Args = serde_json::from_str(arguments)?;
                self.calendar_create_event(args.account_id.as_deref(), &args.summary, &args.start_time, &args.end_time, args.description).await
            }
            _ => anyhow::bail!("Unknown google tool: {tool_name}"),
        }
    }

    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        if !self.get_refresh_token(None).await.unwrap_or_default().is_empty() {
            return Ok(OnboardingStatus::Configured);
        }

        let redirect_uri = "http://localhost:3000/oauth/callback"; 
        let url = Self::generate_auth_url(&self.config, redirect_uri);
        
        Ok(OnboardingStatus::RequiresAction {
            fields: vec![
                OnboardingField {
                    name: "refresh_token".to_string(),
                    label: "Connect Google Account".to_string(),
                    input_type: "oauth".to_string(),
                    value: Some(url),
                    description: Some("Click to authorize Jossie with Google".to_string()),
                }
            ]
        })
    }
}