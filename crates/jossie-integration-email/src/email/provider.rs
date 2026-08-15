impl EmailIntegration {
    async fn do_email_search(
        &self,
        config: &EmailConfig,
        query: Option<&str>,
        terms: &[String],
        match_mode: &str,
        from: Option<&str>,
        subject: Option<&str>,
        after: Option<&str>,
        before: Option<&str>,
        max_results: Option<u32>,
        page_token: Option<&str>,
        folder: &str,
    ) -> anyhow::Result<String> {
        let mut session = Self::imap_connect(config).await?;
        session.select(folder).await?;

        let search_query =
            build_imap_search_query(query, terms, match_mode, from, subject, after, before)?;
        let uids = session.uid_search(&search_query).await?;

        if uids.is_empty() {
            session.logout().await.ok();
            return Ok(serde_json::to_string_pretty(&serde_json::json!({
                "messages": [],
                "next_page_token": serde_json::Value::Null,
            }))?);
        }

        let mut uid_vec: Vec<u32> = uids.into_iter().collect();
        uid_vec.sort_unstable_by(|a, b| b.cmp(a));
        if let Some(cursor) = page_token.and_then(|value| value.parse::<u32>().ok()) {
            uid_vec.retain(|uid| *uid < cursor);
        }
        let max_results = max_results.unwrap_or(20).clamp(1, 100) as usize;
        let has_more = uid_vec.len() > max_results;
        uid_vec.truncate(max_results);
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
        let next_page_token = has_more
            .then(|| uid_vec.last().copied().map(|uid| uid.to_string()))
            .flatten();
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "messages": results,
            "next_page_token": next_page_token,
        }))?)
    }

    async fn do_email_read(
        &self,
        config: &EmailConfig,
        uid: u32,
        folder: &str,
    ) -> anyhow::Result<String> {
        let content = self.do_email_read_content(config, uid, folder).await?;
        Ok(serde_json::json!({
            "uid": content.uid,
            "from": content.from,
            "to": content.to,
            "subject": content.subject,
            "date": content.date,
            "body": content.body,
            "body_source": content.body_source,
            "attachments": content.attachments.iter().map(|attachment| serde_json::json!({
                "part_id": attachment.part_id,
                "filename": attachment.filename,
                "mimeType": attachment.mime_type,
                "size": attachment.size,
            })).collect::<Vec<_>>(),
        })
        .to_string())
    }

    async fn do_email_read_content(
        &self,
        config: &EmailConfig,
        uid: u32,
        folder: &str,
    ) -> anyhow::Result<EmailMessageContent> {
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
                    let to = parse_recipient_list(
                        &parsed.headers.get_first_value("To").unwrap_or_default(),
                    );
                    let date = parsed.headers.get_first_value("Date").unwrap_or_default();
                    let body = extract_message_body(&parsed);
                    let mut attachments = Vec::new();
                    collect_message_attachments(&parsed, "", &mut attachments);
                    EmailMessageContent {
                        uid,
                        from,
                        to,
                        subject,
                        date,
                        body_source: if body.trim().is_empty() {
                            "empty".to_string()
                        } else {
                            "full".to_string()
                        },
                        body: truncate_with_notice(body, MAX_EMAIL_BODY_CHARS),
                        attachments,
                    }
                }
                Err(_) => {
                    let (from, subject, date) = extract_header_fields(raw_message);
                    EmailMessageContent {
                        uid,
                        from,
                        to: Vec::new(),
                        subject,
                        date,
                        body: truncate_with_notice(
                            text_fallback_preview(raw_message),
                            MAX_FALLBACK_PREVIEW_CHARS,
                        ),
                        body_source: "raw_preview".to_string(),
                        attachments: Vec::new(),
                    }
                }
            }
        } else {
            anyhow::bail!("Email not found")
        };

        session.logout().await.ok();
        Ok(result)
    }

    async fn do_email_read_attachment(
        &self,
        config: &EmailConfig,
        uid: u32,
        folder: &str,
        part_id: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let mut session = Self::imap_connect(config).await?;
        session.select(folder).await?;
        let fetch_stream = session.uid_fetch(uid.to_string(), "RFC822").await?;
        let fetched: Vec<_> = {
            use futures::TryStreamExt;
            fetch_stream.try_collect().await?
        };
        let raw_message = fetched
            .first()
            .and_then(|message| message.body())
            .ok_or_else(|| anyhow::anyhow!("Email not found"))?;
        let parsed = mailparse::parse_mail(raw_message)?;
        let part = find_message_attachment(&parsed, part_id)
            .ok_or_else(|| anyhow::anyhow!("Email attachment part not found: {part_id}"))?;
        let bytes = part.get_body_raw()?;
        session.logout().await.ok();
        Ok(bytes)
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

    fn provider_account_id(account_id: &str) -> Option<&str> {
        let account_id = account_id.trim();
        (!account_id.is_empty() && account_id != "default").then_some(account_id)
    }

    pub async fn mail_accounts(&self) -> anyhow::Result<Vec<Value>> {
        Ok(serde_json::from_str(&self.list_accounts().await?)?)
    }

    pub async fn mail_search(
        &self,
        account_id: &str,
        request: EmailSearchRequest,
    ) -> anyhow::Result<Value> {
        let config = self
            .get_account_config(Self::provider_account_id(account_id))
            .await?;
        let folder = request
            .folder
            .as_deref()
            .map(str::trim)
            .filter(|folder| !folder.is_empty())
            .unwrap_or(DEFAULT_FOLDER);
        let result = self
            .do_email_search(
                &config,
                request.query.as_deref(),
                &request.terms,
                &request.match_mode,
                request.from.as_deref(),
                request.subject.as_deref(),
                request.after.as_deref(),
                request.before.as_deref(),
                request.max_results,
                request.page_token.as_deref(),
                folder,
            )
            .await?;
        Ok(serde_json::from_str(&result)?)
    }

    pub async fn mail_read(
        &self,
        account_id: &str,
        uid: u32,
        folder: Option<&str>,
    ) -> anyhow::Result<Value> {
        let config = self
            .get_account_config(Self::provider_account_id(account_id))
            .await?;
        let folder = folder
            .map(str::trim)
            .filter(|folder| !folder.is_empty())
            .unwrap_or(DEFAULT_FOLDER);
        Ok(serde_json::from_str(
            &self.do_email_read(&config, uid, folder).await?,
        )?)
    }

    pub async fn mail_read_content(
        &self,
        account_id: &str,
        uid: u32,
        folder: Option<&str>,
    ) -> anyhow::Result<EmailMessageContent> {
        let config = self
            .get_account_config(Self::provider_account_id(account_id))
            .await?;
        let folder = folder
            .map(str::trim)
            .filter(|folder| !folder.is_empty())
            .unwrap_or(DEFAULT_FOLDER);
        self.do_email_read_content(&config, uid, folder).await
    }

    pub async fn mail_read_attachment(
        &self,
        account_id: &str,
        uid: u32,
        folder: Option<&str>,
        part_id: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let config = self
            .get_account_config(Self::provider_account_id(account_id))
            .await?;
        let folder = folder
            .map(str::trim)
            .filter(|folder| !folder.is_empty())
            .unwrap_or(DEFAULT_FOLDER);
        self.do_email_read_attachment(&config, uid, folder, part_id)
            .await
    }

    pub async fn mail_send(
        &self,
        account_id: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        let config = self
            .get_account_config(Self::provider_account_id(account_id))
            .await?;
        self.do_email_send(&config, to, subject, body).await
    }

    pub async fn mail_folders(&self, account_id: &str) -> anyhow::Result<Vec<String>> {
        let config = self
            .get_account_config(Self::provider_account_id(account_id))
            .await?;
        Ok(serde_json::from_str(&self.do_list_folders(&config).await?)?)
    }
}
