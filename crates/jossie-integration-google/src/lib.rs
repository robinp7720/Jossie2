use jossie_core::config::GoogleConfig;
use jossie_core::integration::{Integration, ToolDefinition};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct GoogleIntegration {
    config: GoogleConfig,
    client: reqwest::Client,
    token: Arc<RwLock<Option<TokenData>>>,
}

#[derive(Clone)]
struct TokenData {
    access_token: String,
    expires_at: std::time::Instant,
}

impl GoogleIntegration {
    pub fn new(config: &GoogleConfig) -> Self {
        Self {
            config: config.clone(),
            client: reqwest::Client::new(),
            token: Arc::new(RwLock::new(None)),
        }
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

    async fn get_access_token(&self) -> anyhow::Result<String> {
        // Check cached token
        {
            let token = self.token.read().await;
            if let Some(ref td) = *token {
                if td.expires_at > std::time::Instant::now() {
                    return Ok(td.access_token.clone());
                }
            }
        }

        // Refresh token
        let resp = self.client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", &self.config.client_id),
                ("client_secret", &self.config.client_secret),
                ("refresh_token", &self.config.refresh_token),
                ("grant_type", &"refresh_token".to_string()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed: {body}");
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

        *self.token.write().await = Some(td);
        Ok(tr.access_token)
    }

    async fn gmail_search(&self, query: &str) -> anyhow::Result<String> {
        let token = self.get_access_token().await?;
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

    async fn gmail_read(&self, message_id: &str) -> anyhow::Result<String> {
        let token = self.get_access_token().await?;
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
        let body_text = extract_body_from_payload(&msg["payload"]);

        Ok(serde_json::json!({
            "id": message_id,
            "from": get_header("From"),
            "to": get_header("To"),
            "subject": get_header("Subject"),
            "date": get_header("Date"),
            "body": body_text,
        }).to_string())
    }

    async fn gmail_send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<String> {
        let token = self.get_access_token().await?;

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

    async fn drive_search(&self, query: &str) -> anyhow::Result<String> {
        let token = self.get_access_token().await?;
        let q = format!("name contains '{}'", query.replace('\'', "\\'"));
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

    async fn drive_read(&self, file_id: &str) -> anyhow::Result<String> {
        let token = self.get_access_token().await?;

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

    async fn calendar_list_events(&self, query: Option<String>, time_min: Option<String>) -> anyhow::Result<String> {
        let token = self.get_access_token().await?;
        let mut req = self.client
            .get("https://www.googleapis.com/calendar/v3/calendars/primary/events")
            .bearer_auth(&token)
            .query(&[("maxResults", "10"), ("singleEvents", "true"), ("orderBy", "startTime")]);
        
        if let Some(q) = query {
            req = req.query(&[("q", q)]);
        }
        if let Some(tm) = time_min {
            req = req.query(&[("timeMin", tm)]);
        } else {
            req = req.query(&[("timeMin", chrono::Utc::now().to_rfc3339())]);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Calendar list failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        let events = data.get("items").unwrap_or(&serde_json::json!([])).clone();
        
        Ok(serde_json::to_string_pretty(&events)?)
    }

    async fn calendar_create_event(&self, summary: &str, start_time: &str, end_time: &str, description: Option<String>) -> anyhow::Result<String> {
        let token = self.get_access_token().await?;
        
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

fn extract_body_from_payload(payload: &serde_json::Value) -> String {
    // Try to find text/plain part
    if let Some(mime) = payload.get("mimeType").and_then(|m| m.as_str()) {
        if mime == "text/plain" {
            if let Some(data) = payload.pointer("/body/data").and_then(|d| d.as_str()) {
                use base64::Engine;
                if let Ok(decoded) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data) {
                    return String::from_utf8_lossy(&decoded).to_string();
                }
            }
        }
    }

    // Check parts recursively
    if let Some(parts) = payload.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            let result = extract_body_from_payload(part);
            if !result.is_empty() {
                return result;
            }
        }
    }

    String::new()
}

#[async_trait::async_trait]
impl Integration for GoogleIntegration {
    fn name(&self) -> &str {
        "google"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "gmail_search".to_string(),
                description: "Search Gmail messages".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
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
        match tool_name {
            "gmail_search" => {
                #[derive(Deserialize)]
                struct Args { query: String }
                let args: Args = serde_json::from_str(arguments)?;
                self.gmail_search(&args.query).await
            }
            "gmail_read" => {
                #[derive(Deserialize)]
                struct Args { message_id: String }
                let args: Args = serde_json::from_str(arguments)?;
                self.gmail_read(&args.message_id).await
            }
            "gmail_send" => {
                #[derive(Deserialize)]
                struct Args { to: String, subject: String, body: String }
                let args: Args = serde_json::from_str(arguments)?;
                self.gmail_send(&args.to, &args.subject, &args.body).await
            }
            "drive_search" => {
                #[derive(Deserialize)]
                struct Args { query: String }
                let args: Args = serde_json::from_str(arguments)?;
                self.drive_search(&args.query).await
            }
            "drive_read" => {
                #[derive(Deserialize)]
                struct Args { file_id: String }
                let args: Args = serde_json::from_str(arguments)?;
                self.drive_read(&args.file_id).await
            }
            "calendar_list_events" => {
                #[derive(Deserialize)]
                struct Args { query: Option<String>, time_min: Option<String> }
                let args: Args = serde_json::from_str(arguments)?;
                self.calendar_list_events(args.query, args.time_min).await
            }
            "calendar_create_event" => {
                #[derive(Deserialize)]
                struct Args { summary: String, start_time: String, end_time: String, description: Option<String> }
                let args: Args = serde_json::from_str(arguments)?;
                self.calendar_create_event(&args.summary, &args.start_time, &args.end_time, args.description).await
            }
            _ => anyhow::bail!("Unknown google tool: {tool_name}"),
        }
    }
}
