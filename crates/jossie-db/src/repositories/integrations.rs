use super::*;

impl Database {
    // Telegram
    pub async fn get_telegram_conversation(&self, chat_id: i64) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query_as::<_, TelegramChatRow>(
            "SELECT conversation_id FROM telegram_chats WHERE telegram_chat_id = ?",
        )
        .bind(chat_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.conversation_id.parse().ok()))
    }

    pub async fn link_telegram_conversation(
        &self,
        chat_id: i64,
        conversation_id: Uuid,
    ) -> anyhow::Result<()> {
        let conv_str = conversation_id.to_string();
        sqlx::query("INSERT OR REPLACE INTO telegram_chats (telegram_chat_id, conversation_id) VALUES (?, ?)")
            .bind(chat_id)
            .bind(&conv_str)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_latest_telegram_chat(&self) -> anyhow::Result<Option<TelegramChatLink>> {
        let row = sqlx::query_as::<_, TelegramChatLatestRow>(
            "SELECT telegram_chat_id, conversation_id FROM telegram_chats ORDER BY created_at DESC LIMIT 1"
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| {
            let conv_id = r.conversation_id.parse().ok()?;
            Some(TelegramChatLink {
                chat_id: r.telegram_chat_id,
                conversation_id: conv_id,
            })
        }))
    }

    pub async fn get_telegram_chat_for_conversation(
        &self,
        conversation_id: Uuid,
    ) -> anyhow::Result<Option<i64>> {
        let conv_str = conversation_id.to_string();
        let row = sqlx::query_as::<_, TelegramChatIdRow>(
            "SELECT telegram_chat_id
             FROM telegram_chats
             WHERE conversation_id = ?
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(&conv_str)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| r.telegram_chat_id))
    }

    // Integration Settings
    pub async fn get_integration_setting(
        &self,
        integration: &str,
        key: &str,
    ) -> anyhow::Result<Option<String>> {
        let row = sqlx::query_as::<_, SettingsRow>(
            "SELECT value FROM integration_settings WHERE integration = ? AND key = ?",
        )
        .bind(integration)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.value))
    }

    pub async fn set_integration_setting(
        &self,
        integration: &str,
        key: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        sqlx::query("INSERT OR REPLACE INTO integration_settings (integration, key, value) VALUES (?, ?, ?)")
            .bind(integration)
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_all_integration_settings(
        &self,
        integration: &str,
    ) -> anyhow::Result<HashMap<String, String>> {
        let rows = sqlx::query_as::<_, SettingsRowAll>(
            "SELECT key, value FROM integration_settings WHERE integration = ?",
        )
        .bind(integration)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| (r.key, r.value)).collect())
    }

    // Integration Accounts
    pub async fn add_integration_account(
        &self,
        integration: &str,
        name: &str,
        data: &serde_json::Value,
    ) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let data_str = serde_json::to_string(data)?;
        sqlx::query(
            "INSERT INTO integration_accounts (id, integration, name, data) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(integration)
        .bind(name)
        .bind(&data_str)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn upsert_integration_account(
        &self,
        id: &str,
        integration: &str,
        name: &str,
        data: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let data_str = serde_json::to_string(data)?;
        sqlx::query("INSERT OR REPLACE INTO integration_accounts (id, integration, name, data) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(integration)
            .bind(name)
            .bind(&data_str)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_integration_account(
        &self,
        id: &str,
        name: &str,
        data: &serde_json::Value,
    ) -> anyhow::Result<bool> {
        let data_str = serde_json::to_string(data)?;
        let result = sqlx::query("UPDATE integration_accounts SET name = ?, data = ? WHERE id = ?")
            .bind(name)
            .bind(data_str)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_integration_account(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<IntegrationAccount>> {
        let row = sqlx::query_as::<_, IntegrationAccount>(
            "SELECT id, integration, name, data, created_at FROM integration_accounts WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn list_integration_accounts(
        &self,
        integration: &str,
    ) -> anyhow::Result<Vec<IntegrationAccount>> {
        let rows = sqlx::query_as::<_, IntegrationAccount>("SELECT id, integration, name, data, created_at FROM integration_accounts WHERE integration = ? ORDER BY created_at ASC")
            .bind(integration)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    pub async fn delete_integration_account(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM integration_accounts WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // Integration Events

    pub async fn insert_integration_event(
        &self,
        integration: &str,
        account_id: &str,
        event_type: &str,
        dedupe_key: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<bool> {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let payload_str = serde_json::to_string(payload)?;
        let res = sqlx::query(
            "INSERT OR IGNORE INTO integration_events (id, integration, account_id, event_type, dedupe_key, payload, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&id)
        .bind(integration)
        .bind(account_id)
        .bind(event_type)
        .bind(dedupe_key)
        .bind(&payload_str)
        .bind(&created_at)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn list_pending_integration_events(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<IntegrationEvent>> {
        let rows = sqlx::query_as::<_, IntegrationEventRow>(
            "SELECT id, integration, account_id, event_type, dedupe_key, payload, status, created_at, processed_at, last_error
             FROM integration_events
             WHERE status = 'new'
             ORDER BY julianday(created_at) ASC, id ASC
             LIMIT ?"
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn mark_integration_event_processed(&self, id: &str) -> anyhow::Result<()> {
        let now_str = Utc::now().to_rfc3339();
        sqlx::query("UPDATE integration_events SET status = 'processed', processed_at = ?, last_error = NULL WHERE id = ?")
            .bind(&now_str)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_integration_event_processing(&self, id: &str) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE integration_events SET status = 'processing' WHERE id = ? AND status = 'new'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn mark_integration_event_new(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE integration_events SET status = 'new', last_error = NULL WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_integration_event_failed(&self, id: &str, error: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE integration_events SET status = 'failed', last_error = ? WHERE id = ?")
            .bind(error)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_stale_processing_integration_events_failed(
        &self,
        before: &str,
        error: &str,
    ) -> anyhow::Result<u64> {
        let now_str = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE integration_events
             SET status = 'failed', processed_at = ?, last_error = ?
             WHERE status = 'processing'
               AND julianday(created_at) < julianday(?)",
        )
        .bind(&now_str)
        .bind(error)
        .bind(before)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
