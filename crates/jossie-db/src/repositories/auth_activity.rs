use super::*;

impl Database {
    pub async fn create_auth_session(&self, token_hash: &str) -> anyhow::Result<AuthSession> {
        let session = AuthSession {
            id: Uuid::new_v4().to_string(),
            token_hash: token_hash.to_string(),
            created_at: Utc::now().to_rfc3339(),
            expires_at: (Utc::now() + Duration::days(30)).to_rfc3339(),
        };
        sqlx::query(
            "INSERT INTO auth_sessions (id, token_hash, expires_at, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&session.id)
        .bind(&session.token_hash)
        .bind(&session.expires_at)
        .bind(&session.created_at)
        .execute(&self.pool)
        .await?;
        Ok(session)
    }

    pub async fn has_valid_auth_session(&self, token_hash: &str) -> anyhow::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let found = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM auth_sessions WHERE token_hash = ? AND expires_at > ? LIMIT 1",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(found.is_some())
    }

    pub async fn revoke_auth_session(&self, token_hash: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM auth_sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn prune_expired_auth_sessions(&self) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM auth_sessions WHERE expires_at <= ?")
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_activity_event(
        &self,
        conversation_id: Option<Uuid>,
        run_id: Option<&str>,
        category: &str,
        title: &str,
        detail: Option<&str>,
        tone: &str,
    ) -> anyhow::Result<ActivityEvent> {
        let event = ActivityEvent {
            id: Uuid::new_v4().to_string(),
            conversation_id,
            run_id: run_id.map(ToOwned::to_owned),
            category: category.to_string(),
            title: title.to_string(),
            detail: detail.map(ToOwned::to_owned),
            tone: tone.to_string(),
            created_at: Utc::now().to_rfc3339(),
        };
        sqlx::query(
            "INSERT INTO activity_events (id, conversation_id, run_id, category, title, detail, tone, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&event.id)
        .bind(event.conversation_id.map(|id| id.to_string()))
        .bind(&event.run_id)
        .bind(&event.category)
        .bind(&event.title)
        .bind(&event.detail)
        .bind(&event.tone)
        .bind(&event.created_at)
        .execute(&self.pool)
        .await?;
        Ok(event)
    }

    pub async fn list_activity_events(
        &self,
        limit: usize,
        before: Option<&str>,
    ) -> anyhow::Result<Vec<ActivityEvent>> {
        let limit = limit.max(1).min(100);
        let before = before.unwrap_or("");
        let rows = sqlx::query_as::<_, ActivityEventRow>(
            "SELECT id, conversation_id, run_id, category, title, detail, tone, created_at
             FROM activity_events
             WHERE (? = '' OR created_at < ?)
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .bind(before)
        .bind(before)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}
