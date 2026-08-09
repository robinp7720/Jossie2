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

    fn last_seen_uid_key(account_id: &str) -> String {
        format!("imap_last_seen_uid:{account_id}")
    }

    fn uid_validity_key(account_id: &str) -> String {
        format!("imap_uid_validity:{account_id}")
    }

    async fn store_mailbox_cursor(
        db: &Arc<Database>,
        account_id: &str,
        last_seen_uid: u32,
        uid_validity: Option<u32>,
    ) -> anyhow::Result<()> {
        db.set_integration_setting(
            EMAIL_INTEGRATION,
            &Self::last_seen_uid_key(account_id),
            &last_seen_uid.to_string(),
        )
        .await?;

        if let Some(uid_validity) = uid_validity {
            db.set_integration_setting(
                EMAIL_INTEGRATION,
                &Self::uid_validity_key(account_id),
                &uid_validity.to_string(),
            )
            .await?;
        }

        Ok(())
    }

    async fn load_mailbox_cursor(
        db: &Arc<Database>,
        account_id: &str,
    ) -> anyhow::Result<(Option<u32>, Option<u32>)> {
        let last_seen_uid = db
            .get_integration_setting(EMAIL_INTEGRATION, &Self::last_seen_uid_key(account_id))
            .await?
            .and_then(|value| value.parse::<u32>().ok());
        let uid_validity = db
            .get_integration_setting(EMAIL_INTEGRATION, &Self::uid_validity_key(account_id))
            .await?
            .and_then(|value| value.parse::<u32>().ok());
        Ok((last_seen_uid, uid_validity))
    }

    fn build_message_unique_id(uid_validity: Option<u32>, uid: u32) -> String {
        match uid_validity {
            Some(uid_validity) => format!("imap:{uid_validity}:{uid}"),
            None => format!("imap:{uid}"),
        }
    }

    fn plan_mailbox_poll(
        stored_last_seen_uid: Option<u32>,
        stored_uid_validity: Option<u32>,
        mailbox_uid_next: Option<u32>,
        mailbox_uid_validity: Option<u32>,
    ) -> MailboxPollAction {
        let current_last_uid = mailbox_uid_next.unwrap_or(1).saturating_sub(1);

        if stored_last_seen_uid.is_none() {
            return MailboxPollAction::SeedCursor {
                last_seen_uid: current_last_uid,
            };
        }

        if let (Some(stored_uid_validity), Some(mailbox_uid_validity)) =
            (stored_uid_validity, mailbox_uid_validity)
        {
            if stored_uid_validity != mailbox_uid_validity {
                return MailboxPollAction::SeedCursor {
                    last_seen_uid: current_last_uid,
                };
            }
        }

        let last_seen_uid = stored_last_seen_uid.unwrap_or_default();
        if let Some(mailbox_uid_next) = mailbox_uid_next {
            if mailbox_uid_next <= last_seen_uid.saturating_add(1) {
                return MailboxPollAction::NoChange;
            }
        }

        MailboxPollAction::PollFrom {
            start_uid: last_seen_uid.saturating_add(1),
        }
    }

    async fn list_poll_accounts(&self) -> anyhow::Result<Vec<PollAccount>> {
        let mut accounts = Vec::new();

        if let Some(config) = &self.default_config {
            accounts.push(PollAccount {
                id: "default".to_string(),
                email: config.username.clone(),
                config: config.clone(),
            });
        }

        if let Some(db) = &self.db {
            for account in db.list_integration_accounts(EMAIL_INTEGRATION).await? {
                if let Ok(config) = serde_json::from_str::<EmailConfig>(&account.data) {
                    if !config.imap_host.trim().is_empty() {
                        accounts.push(Self::poll_account_from_db(account, config));
                    }
                }
            }
        }

        Ok(accounts)
    }

    fn poll_account_from_db(account: IntegrationAccount, config: EmailConfig) -> PollAccount {
        PollAccount {
            id: account.id,
            email: config.username.clone(),
            config,
        }
    }

    async fn current_mailbox_state(
        &self,
        config: &EmailConfig,
    ) -> anyhow::Result<(Option<u32>, Option<u32>)> {
        let mut session = Self::imap_connect(config).await?;
        let mailbox = session
            .status(DEFAULT_FOLDER, "(UIDNEXT UIDVALIDITY)")
            .await?;
        session.logout().await.ok();
        Ok((mailbox.uid_next, mailbox.uid_validity))
    }

    async fn seed_mailbox_cursor(
        &self,
        config: &EmailConfig,
        fallback_last_seen_uid: u32,
    ) -> anyhow::Result<(u32, Option<u32>)> {
        let mut session = Self::imap_connect(config).await?;
        let mailbox = session.select(DEFAULT_FOLDER).await?;
        let last_seen_uid = match mailbox.uid_next {
            Some(uid_next) => uid_next.saturating_sub(1),
            None => {
                let mut uids: Vec<u32> = session.uid_search("ALL").await?.into_iter().collect();
                uids.sort_unstable();
                uids.last().copied().unwrap_or(fallback_last_seen_uid)
            }
        };
        let uid_validity = mailbox.uid_validity;
        session.logout().await.ok();
        Ok((last_seen_uid, uid_validity))
    }

    async fn fetch_new_message_summaries(
        &self,
        config: &EmailConfig,
        start_uid: u32,
    ) -> anyhow::Result<(Vec<ImapEventSummary>, Option<u32>)> {
        let mut session = Self::imap_connect(config).await?;
        let mailbox = session.select(DEFAULT_FOLDER).await?;
        let uid_validity = mailbox.uid_validity;

        let query = format!("UID {start_uid}:*");
        let mut uids: Vec<u32> = session.uid_search(&query).await?.into_iter().collect();
        uids.retain(|uid| *uid >= start_uid);
        uids.sort_unstable();

        if uids.is_empty() {
            session.logout().await.ok();
            return Ok((Vec::new(), uid_validity));
        }

        if uids.len() > MAX_POLL_FETCH_UIDS {
            uids.truncate(MAX_POLL_FETCH_UIDS);
        }

        let uid_set = uids
            .iter()
            .map(|uid| uid.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let fetch_stream = session.uid_fetch(&uid_set, "RFC822.HEADER").await?;
        let fetched: Vec<_> = {
            use futures::TryStreamExt;
            fetch_stream.try_collect().await?
        };

        let mut summaries = Vec::new();
        for uid in uids {
            let Some(message) = fetched.iter().find(|message| message.uid == Some(uid)) else {
                continue;
            };

            let header = message
                .header()
                .or_else(|| message.body())
                .unwrap_or_default();
            let parsed = parse_header_summary(header);
            let message_unique_id = Self::build_message_unique_id(uid_validity, uid);
            summaries.push(ImapEventSummary {
                uid,
                message_unique_id,
                header_message_id: parsed.message_id,
                from: parsed.from,
                to: parsed.to,
                subject: parsed.subject,
                date: parsed.date,
            });
        }

        session.logout().await.ok();
        Ok((summaries, uid_validity))
    }

    async fn emit_new_email_events(
        &self,
        db: &Arc<Database>,
        account: &PollAccount,
        messages: &[ImapEventSummary],
    ) -> anyhow::Result<()> {
        for message in messages {
            let payload = serde_json::json!({
                "uid": message.uid,
                "message_id": &message.header_message_id,
                "message_unique_id": &message.message_unique_id,
                "from": &message.from,
                "to": &message.to,
                "subject": &message.subject,
                "date": &message.date,
                "received_at": Utc::now().to_rfc3339(),
                "folder": DEFAULT_FOLDER,
                "event_semantics": "new_message_arrival",
                "account_id": &account.id,
                "account_email": &account.email,
            });
            db.insert_integration_event(
                EMAIL_INTEGRATION,
                &account.id,
                "new_email",
                &message.message_unique_id,
                &payload,
            )
            .await?;
        }

        Ok(())
    }

    async fn poll_account(&self, db: &Arc<Database>, account: &PollAccount) -> anyhow::Result<()> {
        let (mailbox_uid_next, mailbox_uid_validity) =
            self.current_mailbox_state(&account.config).await?;
        let (stored_last_seen_uid, stored_uid_validity) =
            Self::load_mailbox_cursor(db, &account.id).await?;

        match Self::plan_mailbox_poll(
            stored_last_seen_uid,
            stored_uid_validity,
            mailbox_uid_next,
            mailbox_uid_validity,
        ) {
            MailboxPollAction::NoChange => {}
            MailboxPollAction::SeedCursor { last_seen_uid } => {
                let (seeded_last_seen_uid, seeded_uid_validity) = self
                    .seed_mailbox_cursor(&account.config, last_seen_uid)
                    .await?;
                Self::store_mailbox_cursor(
                    db,
                    &account.id,
                    seeded_last_seen_uid,
                    seeded_uid_validity.or(mailbox_uid_validity),
                )
                .await?;
            }
            MailboxPollAction::PollFrom { start_uid } => {
                let (messages, fetched_uid_validity) = self
                    .fetch_new_message_summaries(&account.config, start_uid)
                    .await?;

                if let Some(max_uid) = messages.iter().map(|message| message.uid).max() {
                    self.emit_new_email_events(db, account, &messages).await?;
                    Self::store_mailbox_cursor(
                        db,
                        &account.id,
                        max_uid,
                        fetched_uid_validity.or(mailbox_uid_validity),
                    )
                    .await?;
                } else {
                    let last_seen_uid = mailbox_uid_next
                        .unwrap_or(start_uid)
                        .saturating_sub(1)
                        .max(start_uid.saturating_sub(1));
                    Self::store_mailbox_cursor(
                        db,
                        &account.id,
                        last_seen_uid,
                        fetched_uid_validity.or(mailbox_uid_validity),
                    )
                    .await?;
                }
            }
        }

        Ok(())
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

}
