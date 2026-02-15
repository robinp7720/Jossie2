use jossie_core::config::EmailConfig;
use jossie_core::integration::{Integration, OnboardingField, OnboardingStatus, ToolDefinition};
use jossie_db::Database;
use mailparse::{DispositionType, MailHeaderMap, ParsedMail};
use serde::Deserialize;
use std::sync::Arc;

type ImapSession = async_imap::Session<
    async_native_tls::TlsStream<tokio_util::compat::Compat<tokio::net::TcpStream>>,
>;

const DEFAULT_FOLDER: &str = "INBOX";
const MAX_EMAIL_BODY_CHARS: usize = 60_000;
const MAX_FALLBACK_PREVIEW_CHARS: usize = 4_000;

pub struct EmailIntegration {
    default_config: Option<EmailConfig>,
    db: Option<Arc<Database>>,
}

impl EmailIntegration {
    pub fn new(config: &EmailConfig) -> Self {
        let default_config = if !config.imap_host.is_empty() {
            Some(config.clone())
        } else {
            None
        };
        Self {
            default_config,
            db: None,
        }
    }

    pub fn set_db(&mut self, db: Arc<Database>) {
        self.db = Some(db);
    }

    async fn get_account_config(&self, account_id: Option<&str>) -> anyhow::Result<EmailConfig> {
        match account_id {
            Some(id) if id != "default" => {
                if let Some(db) = &self.db {
                    if let Some(acc) = db.get_integration_account(id).await? {
                        let config: EmailConfig = serde_json::from_str(&acc.data)?;
                        return Ok(config);
                    }
                }
                anyhow::bail!("Account not found: {}", id)
            }
            _ => self
                .default_config
                .clone()
                .ok_or_else(|| anyhow::anyhow!("No default email account configured")),
        }
    }

    async fn imap_connect(config: &EmailConfig) -> anyhow::Result<ImapSession> {
        use tokio_util::compat::TokioAsyncReadCompatExt;
        let tcp = tokio::net::TcpStream::connect((&*config.imap_host, config.imap_port)).await?;
        let tls = async_native_tls::TlsConnector::new();
        let tls_stream = tls.connect(&config.imap_host, tcp.compat()).await?;
        let client = async_imap::Client::new(tls_stream);
        let session = client
            .login(&config.username, &config.password)
            .await
            .map_err(|e| anyhow::anyhow!("IMAP login failed: {}", e.0))?;
        Ok(session)
    }

    async fn do_email_search(
        &self,
        config: &EmailConfig,
        query: &str,
        folder: &str,
    ) -> anyhow::Result<String> {
        let mut session = Self::imap_connect(config).await?;
        session.select(folder).await?;

        let escaped_query = escape_imap_query_value(query);
        let search_query = format!(
            "OR SUBJECT \"{}\" FROM \"{}\"",
            escaped_query, escaped_query
        );
        let uids = session.uid_search(&search_query).await?;

        if uids.is_empty() {
            session.logout().await.ok();
            return Ok("No matching emails found.".to_string());
        }

        let mut uid_vec: Vec<u32> = uids.into_iter().collect();
        uid_vec.sort_unstable_by(|a, b| b.cmp(a));
        uid_vec.truncate(20);
        let uid_set: String = uid_vec
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");

        let fetch_stream = session.uid_fetch(&uid_set, "RFC822.HEADER").await?;
        let fetched: Vec<_> = {
            use futures::TryStreamExt;
            fetch_stream.try_collect().await?
        };

        let mut results = Vec::new();
        for msg in &fetched {
            let uid = msg.uid.unwrap_or(0);
            let header = msg.header().or_else(|| msg.body()).unwrap_or_default();
            let header_str = String::from_utf8_lossy(header).trim().to_string();
            let (from, subject, date) = extract_header_fields(header);
            results.push(serde_json::json!({
                "uid": uid,
                "from": from,
                "subject": subject,
                "date": date,
                "headers": header_str,
            }));
        }

        session.logout().await.ok();
        Ok(serde_json::to_string_pretty(&results)?)
    }

    async fn do_email_read(
        &self,
        config: &EmailConfig,
        uid: u32,
        folder: &str,
    ) -> anyhow::Result<String> {
        let mut session = Self::imap_connect(config).await?;
        session.select(folder).await?;

        let fetch_stream = session.uid_fetch(uid.to_string(), "RFC822").await?;
        let fetched: Vec<_> = {
            use futures::TryStreamExt;
            fetch_stream.try_collect().await?
        };

        let result = if let Some(msg) = fetched.first() {
            let raw_message = msg.body().unwrap_or_default();
            match mailparse::parse_mail(raw_message) {
                Ok(parsed) => {
                    let subject = parsed
                        .headers
                        .get_first_value("Subject")
                        .unwrap_or_default();
                    let from = parsed.headers.get_first_value("From").unwrap_or_default();
                    let date = parsed.headers.get_first_value("Date").unwrap_or_default();
                    let body_text =
                        truncate_with_notice(extract_message_body(&parsed), MAX_EMAIL_BODY_CHARS);
                    serde_json::json!({
                        "uid": uid,
                        "from": from,
                        "subject": subject,
                        "date": date,
                        "body": body_text,
                    })
                    .to_string()
                }
                Err(_) => {
                    let (from, subject, date) = extract_header_fields(raw_message);
                    serde_json::json!({
                        "uid": uid,
                        "from": from,
                        "subject": subject,
                        "date": date,
                        "body": truncate_with_notice(text_fallback_preview(raw_message), MAX_FALLBACK_PREVIEW_CHARS),
                        "note": "Email body parsing failed; returned a trimmed raw preview instead.",
                    })
                    .to_string()
                }
            }
        } else {
            "Email not found".to_string()
        };

        session.logout().await.ok();
        Ok(result)
    }

    async fn do_email_send(
        &self,
        config: &EmailConfig,
        to: &str,
        subject: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        use lettre::{
            AsyncSmtpTransport, AsyncTransport, Message as LettreMessage, Tokio1Executor,
            transport::smtp::authentication::Credentials,
        };

        let email = LettreMessage::builder()
            .from(config.username.parse()?)
            .to(to.parse()?)
            .subject(subject)
            .body(body.to_string())?;

        let creds = Credentials::new(config.username.clone(), config.password.clone());

        let mailer: AsyncSmtpTransport<Tokio1Executor> =
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)?
                .port(config.smtp_port)
                .credentials(creds)
                .build();

        mailer.send(email).await?;
        Ok(format!("Email sent to {to}"))
    }

    async fn do_list_folders(&self, config: &EmailConfig) -> anyhow::Result<String> {
        let mut session = Self::imap_connect(config).await?;

        let list_stream = session.list(None, Some("*")).await?;
        let mailboxes: Vec<_> = {
            use futures::TryStreamExt;
            list_stream.try_collect().await?
        };

        let folders: Vec<String> = mailboxes.iter().map(|mb| mb.name().to_string()).collect();

        session.logout().await.ok();
        Ok(serde_json::to_string_pretty(&folders)?)
    }

    async fn list_accounts(&self) -> anyhow::Result<String> {
        let mut accounts = Vec::new();
        if let Some(config) = &self.default_config {
            accounts.push(serde_json::json!({
                "id": "default",
                "name": "Default Config",
                "email": config.username
            }));
        }
        if let Some(db) = &self.db {
            let db_accounts = db.list_integration_accounts("email").await?;
            for acc in db_accounts {
                if let Ok(cfg) = serde_json::from_str::<EmailConfig>(&acc.data) {
                    accounts.push(serde_json::json!({
                        "id": acc.id,
                        "name": acc.name,
                        "email": cfg.username
                    }));
                }
            }
        }
        Ok(serde_json::to_string_pretty(&accounts)?)
    }
}

#[async_trait::async_trait]
impl Integration for EmailIntegration {
    fn name(&self) -> &str {
        "email"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "email_list_accounts".to_string(),
                description: "List configured email accounts".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "email_search".to_string(),
                description: "Search emails by query in subject or sender".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID (use empty string for default account)"},
                        "query": {"type": "string", "description": "Search term"},
                        "folder": {"type": "string", "description": "IMAP folder to search (use empty string for INBOX)"}
                    },
                    "required": ["account_id", "query", "folder"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "email_read".to_string(),
                description: "Read a specific email by UID".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID (use empty string for default account)"},
                        "uid": {"type": "integer", "description": "Email UID from search results"},
                        "folder": {"type": "string", "description": "IMAP folder (use empty string for INBOX)"}
                    },
                    "required": ["account_id", "uid", "folder"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "email_send".to_string(),
                description: "Send an email".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID (use empty string for default account)"},
                        "to": {"type": "string", "description": "Recipient email address"},
                        "subject": {"type": "string", "description": "Email subject"},
                        "body": {"type": "string", "description": "Email body text"}
                    },
                    "required": ["account_id", "to", "subject", "body"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "email_list_folders".to_string(),
                description: "List all email folders/mailboxes".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID (use empty string for default account)"}
                    },
                    "required": ["account_id"],
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        tracing::debug!("email.execute: {tool_name}");
        if tool_name == "email_list_accounts" {
            return self.list_accounts().await;
        }

        // Common args struct for account extraction
        #[derive(Deserialize)]
        struct AccountArgs {
            #[serde(default)]
            account_id: String,
        }
        let base_args: AccountArgs = serde_json::from_str(arguments).unwrap_or(AccountArgs {
            account_id: String::new(),
        });
        let account_id = base_args.account_id.trim();
        let account_id = if account_id.is_empty() || account_id == "default" {
            None
        } else {
            Some(account_id)
        };
        let config = self.get_account_config(account_id).await?;

        match tool_name {
            "email_search" => {
                #[derive(Deserialize)]
                struct Args {
                    query: String,
                    #[serde(default)]
                    folder: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let folder = if args.folder.trim().is_empty() {
                    DEFAULT_FOLDER.to_string()
                } else {
                    args.folder
                };
                self.do_email_search(&config, &args.query, &folder).await
            }
            "email_read" => {
                #[derive(Deserialize)]
                struct Args {
                    uid: u32,
                    #[serde(default)]
                    folder: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let folder = if args.folder.trim().is_empty() {
                    DEFAULT_FOLDER.to_string()
                } else {
                    args.folder
                };
                self.do_email_read(&config, args.uid, &folder).await
            }
            "email_send" => {
                #[derive(Deserialize)]
                struct Args {
                    to: String,
                    subject: String,
                    body: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.do_email_send(&config, &args.to, &args.subject, &args.body)
                    .await
            }
            "email_list_folders" => self.do_list_folders(&config).await,
            _ => anyhow::bail!("Unknown email tool: {tool_name}"),
        }
    }

    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        // If default config exists, we are good.
        if self.default_config.is_some() {
            return Ok(OnboardingStatus::Configured);
        }
        // If DB has accounts, we are good.
        if let Some(db) = &self.db {
            let accounts = db.list_integration_accounts("email").await?;
            if !accounts.is_empty() {
                return Ok(OnboardingStatus::Configured);
            }
        }

        // Otherwise, need setup
        Ok(OnboardingStatus::RequiresAction {
            fields: vec![
                OnboardingField {
                    name: "note".to_string(),
                    label: "Setup Email".to_string(),
                    input_type: "info".to_string(),
                    value: None,
                    description: Some("No email accounts configured. Add one via the settings API (implementation pending) or config.toml.".to_string()),
                }
            ]
        })
    }
}

fn escape_imap_query_value(value: &str) -> String {
    value.replace('\\', r"\\").replace('"', r#"\""#)
}

fn extract_header_fields(header_bytes: &[u8]) -> (String, String, String) {
    match mailparse::parse_headers(header_bytes) {
        Ok((headers, _)) => (
            headers.get_first_value("From").unwrap_or_default(),
            headers.get_first_value("Subject").unwrap_or_default(),
            headers.get_first_value("Date").unwrap_or_default(),
        ),
        Err(_) => (String::new(), String::new(), String::new()),
    }
}

fn truncate_with_notice(text: String, max_chars: usize) -> String {
    jossie_core::text::truncate_with_notice(text, max_chars)
}

fn text_fallback_preview(raw_message: &[u8]) -> String {
    let preview_len = raw_message.len().min(32_000);
    let preview = String::from_utf8_lossy(&raw_message[..preview_len]);
    // html_to_text also collapses whitespace, so it works on plain text too
    jossie_core::text::html_to_text(&preview)
}

fn extract_message_body(parsed: &ParsedMail<'_>) -> String {
    let mut text_parts = Vec::new();
    let mut html_parts = Vec::new();
    collect_message_parts(parsed, &mut text_parts, &mut html_parts);

    if !text_parts.is_empty() {
        return text_parts.join("\n\n").trim().to_string();
    }

    if !html_parts.is_empty() {
        return html_parts.join("\n\n").trim().to_string();
    }

    parsed.get_body().unwrap_or_default().trim().to_string()
}

fn collect_message_parts(
    part: &ParsedMail<'_>,
    text_parts: &mut Vec<String>,
    html_parts: &mut Vec<String>,
) {
    if part.get_content_disposition().disposition == DispositionType::Attachment {
        return;
    }

    if part.subparts.is_empty() {
        let mime = part.ctype.mimetype.to_ascii_lowercase();
        if mime == "text/plain" {
            if let Ok(body) = part.get_body() {
                let body = body.trim();
                if !body.is_empty() {
                    text_parts.push(body.to_string());
                }
            }
        } else if mime == "text/html" {
            if let Ok(body) = part.get_body() {
                let body = html_to_text(&body);
                if !body.is_empty() {
                    html_parts.push(body);
                }
            }
        }
        return;
    }

    for child in &part.subparts {
        collect_message_parts(child, text_parts, html_parts);
    }
}

fn html_to_text(html: &str) -> String {
    jossie_core::text::html_to_text(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_message_body_prefers_plaintext() {
        let raw = concat!(
            "Subject: Test\r\n",
            "From: sender@example.com\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/alternative; boundary=\"ALT\"\r\n",
            "\r\n",
            "--ALT\r\n",
            "Content-Type: text/plain; charset=UTF-8\r\n",
            "\r\n",
            "Hello from plain text.\r\n",
            "--ALT\r\n",
            "Content-Type: text/html; charset=UTF-8\r\n",
            "\r\n",
            "<html><body><p>Hello from <b>HTML</b>.</p></body></html>\r\n",
            "--ALT--\r\n"
        );

        let parsed = mailparse::parse_mail(raw.as_bytes()).expect("mail should parse");
        let body = extract_message_body(&parsed);
        assert!(body.contains("Hello from plain text."));
    }

    #[test]
    fn extract_message_body_falls_back_to_html() {
        let raw = concat!(
            "Subject: HTML only\r\n",
            "From: sender@example.com\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: text/html; charset=UTF-8\r\n",
            "\r\n",
            "<html><body><h1>Meeting</h1><p>Tomorrow at 10:00.</p></body></html>\r\n"
        );

        let parsed = mailparse::parse_mail(raw.as_bytes()).expect("mail should parse");
        let body = extract_message_body(&parsed);
        assert!(body.contains("Meeting"));
        assert!(body.contains("Tomorrow at 10:00."));
    }

    #[test]
    fn extract_header_fields_reads_common_headers() {
        let raw = concat!(
            "From: sender@example.com\r\n",
            "Subject: Subject line\r\n",
            "Date: Tue, 10 Feb 2026 09:00:00 +0000\r\n",
            "\r\n"
        );

        let (from, subject, date) = extract_header_fields(raw.as_bytes());
        assert_eq!(from, "sender@example.com");
        assert_eq!(subject, "Subject line");
        assert_eq!(date, "Tue, 10 Feb 2026 09:00:00 +0000");
    }
}
