use jossie_core::config::EmailConfig;
use jossie_core::integration::{Integration, ToolDefinition};
use serde::Deserialize;

type ImapSession = async_imap::Session<async_native_tls::TlsStream<tokio_util::compat::Compat<tokio::net::TcpStream>>>;

pub struct EmailIntegration {
    config: EmailConfig,
}

impl EmailIntegration {
    pub fn new(config: &EmailConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    async fn imap_connect(&self) -> anyhow::Result<ImapSession> {
        use tokio_util::compat::TokioAsyncReadCompatExt;
        let tcp = tokio::net::TcpStream::connect((&*self.config.imap_host, self.config.imap_port)).await?;
        let tls = async_native_tls::TlsConnector::new();
        let tls_stream = tls.connect(&self.config.imap_host, tcp.compat()).await?;
        let client = async_imap::Client::new(tls_stream);
        let session = client.login(&self.config.username, &self.config.password).await
            .map_err(|e| anyhow::anyhow!("IMAP login failed: {}", e.0))?;
        Ok(session)
    }

    async fn do_email_search(&self, query: &str, folder: &str) -> anyhow::Result<String> {
        let mut session = self.imap_connect().await?;
        session.select(folder).await?;

        let search_query = format!("OR SUBJECT \"{}\" FROM \"{}\"", query, query);
        let uids = session.uid_search(&search_query).await?;

        if uids.is_empty() {
            session.logout().await.ok();
            return Ok("No matching emails found.".to_string());
        }

        // HashSet -> sorted Vec (most recent UIDs first)
        let mut uid_vec: Vec<u32> = uids.into_iter().collect();
        uid_vec.sort_unstable_by(|a, b| b.cmp(a));
        uid_vec.truncate(20);
        let uid_set: String = uid_vec.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(",");

        let fetch_stream = session.uid_fetch(&uid_set, "BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE)]").await?;
        // Collect all results before dropping the borrow on session
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

    async fn do_email_read(&self, uid: u32, folder: &str) -> anyhow::Result<String> {
        let mut session = self.imap_connect().await?;
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
                    let subject = parsed.headers.iter()
                        .find(|h| h.get_key_ref() == "Subject")
                        .map(|h| h.get_value())
                        .unwrap_or_default();
                    let from = parsed.headers.iter()
                        .find(|h| h.get_key_ref() == "From")
                        .map(|h| h.get_value())
                        .unwrap_or_default();
                    let date = parsed.headers.iter()
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
                    }).to_string()
                }
                Err(_) => String::from_utf8_lossy(body).to_string(),
            }
        } else {
            "Email not found".to_string()
        };

        session.logout().await.ok();
        Ok(result)
    }

    async fn do_email_send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<String> {
        use lettre::{
            Message as LettreMessage, AsyncSmtpTransport, AsyncTransport,
            transport::smtp::authentication::Credentials,
            Tokio1Executor,
        };

        let email = LettreMessage::builder()
            .from(self.config.username.parse()?)
            .to(to.parse()?)
            .subject(subject)
            .body(body.to_string())?;

        let creds = Credentials::new(
            self.config.username.clone(),
            self.config.password.clone(),
        );

        let mailer: AsyncSmtpTransport<Tokio1Executor> = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.config.smtp_host)?
            .port(self.config.smtp_port)
            .credentials(creds)
            .build();

        mailer.send(email).await?;
        Ok(format!("Email sent to {to}"))
    }

    async fn do_list_folders(&self) -> anyhow::Result<String> {
        let mut session = self.imap_connect().await?;

        let list_stream = session.list(None, Some("*")).await?;
        let mailboxes: Vec<_> = {
            use futures::TryStreamExt;
            list_stream.try_collect().await?
        };

        let folders: Vec<String> = mailboxes.iter().map(|mb| mb.name().to_string()).collect();

        session.logout().await.ok();
        Ok(serde_json::to_string_pretty(&folders)?)
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
                name: "email_search".to_string(),
                description: "Search emails by query in subject or sender".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search term"},
                        "folder": {"type": "string", "description": "IMAP folder to search (default: INBOX)"}
                    },
                    "required": ["query"]
                }),
            },
            ToolDefinition {
                name: "email_read".to_string(),
                description: "Read a specific email by UID".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "uid": {"type": "integer", "description": "Email UID from search results"},
                        "folder": {"type": "string", "description": "IMAP folder (default: INBOX)"}
                    },
                    "required": ["uid"]
                }),
            },
            ToolDefinition {
                name: "email_send".to_string(),
                description: "Send an email".to_string(),
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
                name: "email_list_folders".to_string(),
                description: "List all email folders/mailboxes".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        tracing::debug!("email.execute: {tool_name}");
        match tool_name {
            "email_search" => {
                #[derive(Deserialize)]
                struct Args { query: String, #[serde(default = "default_folder")] folder: String }
                fn default_folder() -> String { "INBOX".to_string() }
                let args: Args = serde_json::from_str(arguments)?;
                self.do_email_search(&args.query, &args.folder).await
            }
            "email_read" => {
                #[derive(Deserialize)]
                struct Args { uid: u32, #[serde(default = "default_folder")] folder: String }
                fn default_folder() -> String { "INBOX".to_string() }
                let args: Args = serde_json::from_str(arguments)?;
                self.do_email_read(args.uid, &args.folder).await
            }
            "email_send" => {
                #[derive(Deserialize)]
                struct Args { to: String, subject: String, body: String }
                let args: Args = serde_json::from_str(arguments)?;
                self.do_email_send(&args.to, &args.subject, &args.body).await
            }
            "email_list_folders" => {
                self.do_list_folders().await
            }
            _ => anyhow::bail!("Unknown email tool: {tool_name}"),
        }
    }
}
