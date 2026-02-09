use jossie_core::config::EmailConfig;
use jossie_core::integration::{Integration, OnboardingField, OnboardingStatus, ToolDefinition};
use jossie_db::Database;
use serde::Deserialize;
use std::sync::Arc;

type ImapSession = async_imap::Session<
    async_native_tls::TlsStream<tokio_util::compat::Compat<tokio::net::TcpStream>>,
>;

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

        let search_query = format!("OR SUBJECT \"{}\" FROM \"{}\"", query, query);
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

        let fetch_stream = session
            .uid_fetch(&uid_set, "BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE)]")
            .await?;
        let fetched: Vec<_> = {
            use futures::TryStreamExt;
            fetch_stream.try_collect().await?
        };

        let mut results = Vec::new();
        for msg in &fetched {
            let uid = msg.uid.unwrap_or(0);
            let header = msg.body().unwrap_or_default();
            let header_str = String::from_utf8_lossy(header);
            results.push(serde_json::json!({
                "uid": uid,
                "headers": header_str.trim(),
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
            let body = msg.body().unwrap_or_default();
            match mailparse::parse_mail(body) {
                Ok(parsed) => {
                    let subject = parsed
                        .headers
                        .iter()
                        .find(|h| h.get_key_ref() == "Subject")
                        .map(|h| h.get_value())
                        .unwrap_or_default();
                    let from = parsed
                        .headers
                        .iter()
                        .find(|h| h.get_key_ref() == "From")
                        .map(|h| h.get_value())
                        .unwrap_or_default();
                    let date = parsed
                        .headers
                        .iter()
                        .find(|h| h.get_key_ref() == "Date")
                        .map(|h| h.get_value())
                        .unwrap_or_default();
                    let body_text = parsed.get_body().unwrap_or_default();
                    serde_json::json!({
                        "uid": uid,
                        "from": from,
                        "subject": subject,
                        "date": date,
                        "body": body_text,
                    })
                    .to_string()
                }
                Err(_) => String::from_utf8_lossy(body).to_string(),
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
                fn default_folder() -> String {
                    "INBOX".to_string()
                }
                let args: Args = serde_json::from_str(arguments)?;
                let folder = if args.folder.trim().is_empty() {
                    default_folder()
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
                fn default_folder() -> String {
                    "INBOX".to_string()
                }
                let args: Args = serde_json::from_str(arguments)?;
                let folder = if args.folder.trim().is_empty() {
                    default_folder()
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
