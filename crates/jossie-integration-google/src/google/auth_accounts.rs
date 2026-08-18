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

    /// Check if a poll error is an invalid_grant, and if so pause the account
    /// and queue a reconnect notice. Returns `true` if the account was paused.
    async fn handle_poll_invalid_grant(
        &self,
        db: &Arc<Database>,
        acc: &IntegrationAccount,
        error: &anyhow::Error,
    ) -> anyhow::Result<bool> {
        if !Self::is_invalid_grant_text(&error.to_string()) {
            return Ok(false);
        }
        tracing::warn!(
            "Pausing Google account {} due to invalid_grant token refresh failure",
            acc.id
        );
        self.pause_account_invalid_grant(db, acc, &error.to_string())
            .await?;
        if let Err(notice_err) = self.queue_reconnect_notice_if_due(db, acc).await {
            tracing::warn!(
                "Failed to queue reconnect reminder for account {}: {notice_err}",
                acc.id
            );
        }
        Ok(true)
    }

    fn get_account_refresh_token(acc: &IntegrationAccount) -> Option<String> {
        serde_json::from_str::<StoredAccount>(&acc.data)
            .ok()
            .map(|data| data.refresh_token)
            .filter(|token| !token.trim().is_empty())
    }

    fn refresh_token_fingerprint(token: &str) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(token.as_bytes()))
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
        let current_refresh = Self::get_account_refresh_token(acc)
            .map(|token| Self::refresh_token_fingerprint(&token));
        if let (Some(paused), Some(current)) = (paused_refresh, current_refresh)
            && !paused.is_empty()
            && paused != current
        {
            tracing::info!(
                "Google account {} refresh token changed; clearing paused status",
                acc.id
            );
            self.clear_account_pause_state(db, &acc.id).await?;
            return Ok(false);
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

        let refresh = Self::get_account_refresh_token(acc)
            .map(|token| Self::refresh_token_fingerprint(&token))
            .unwrap_or_default();
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
        if let Some(last_notice) = last_notice
            && let Ok(last_dt) = DateTime::parse_from_rfc3339(&last_notice)
        {
            let cooldown = Duration::hours(RECONNECT_NOTICE_COOLDOWN_HOURS);
            if Utc::now() - last_dt.with_timezone(&Utc) < cooldown {
                return Ok(());
            }
        }

        let Some(conversation_id) = db.get_latest_conversation_id().await? else {
            return Ok(());
        };

        let message = format!(
            "Google account '{}' needs reconnect: refresh token was expired/revoked. Please reconnect it in settings.",
            acc.name
        );
        db.queue_oob_message(conversation_id, &message, "high")
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
            "https://www.googleapis.com/auth/tasks",
            "https://www.googleapis.com/auth/contacts.readonly",
        ]
        .join(" ");

        let mut url = format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
            urlencoding::encode(&config.client_id),
            urlencoding::encode(redirect_uri),
            urlencoding::encode(&scopes),
        );

        if let Some(state) = state {
            url.push_str("&state=");
            url.push_str(&urlencoding::encode(state));
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
            if let Some(td) = tokens.get(&account_key)
                && td.expires_at > std::time::Instant::now()
            {
                return Ok(td.access_token.clone());
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

    pub async fn account_values(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        tracing::debug!("Listing Google integration accounts");
        let mut accounts = Vec::new();

        if let Some(db) = &self.db {
            let db_accounts = db.list_integration_accounts("google").await?;
            for acc in db_accounts {
                tracing::debug!("Listing Google account: {} - {}", acc.id, acc.name);
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
        Ok(accounts)
    }

    pub async fn list_accounts(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(&self.account_values().await?)?)
    }

}
