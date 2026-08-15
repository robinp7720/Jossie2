use super::*;

impl Database {
    // Scheduled Tasks

    #[allow(clippy::too_many_arguments)]
    pub async fn create_scheduled_task(
        &self,
        conversation_id: Uuid,
        task_type: &str,
        task_data: &serde_json::Value,
        schedule_type: &str,
        schedule_value: &str,
        next_run_at: Option<&str>,
        max_runs: Option<i64>,
    ) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let conv_str = conversation_id.to_string();
        let task_data_str = serde_json::to_string(task_data)?;
        let now_str = Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO scheduled_tasks (id, conversation_id, task_type, task_data, schedule_type, schedule_value, next_run_at, max_runs, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&id)
        .bind(&conv_str)
        .bind(task_type)
        .bind(&task_data_str)
        .bind(schedule_type)
        .bind(schedule_value)
        .bind(next_run_at)
        .bind(max_runs)
        .bind(&now_str)
        .bind(&now_str)
        .execute(&self.pool)
        .await?;

        Ok(id)
    }

    pub async fn get_scheduled_task(&self, id: &str) -> anyhow::Result<Option<ScheduledTask>> {
        let row = sqlx::query_as::<_, ScheduledTaskRow>(
            "SELECT id, conversation_id, task_type, task_data, schedule_type, schedule_value, status, next_run_at, last_run_at, run_count, max_runs, created_at, updated_at, last_error
             FROM scheduled_tasks WHERE id = ?"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn list_pending_scheduled_tasks(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<ScheduledTask>> {
        let now_str = Utc::now().to_rfc3339();
        let rows = sqlx::query_as::<_, ScheduledTaskRow>(
            "SELECT id, conversation_id, task_type, task_data, schedule_type, schedule_value, status, next_run_at, last_run_at, run_count, max_runs, created_at, updated_at, last_error
             FROM scheduled_tasks
             WHERE status = 'pending' AND (next_run_at IS NULL OR next_run_at <= ?)
             ORDER BY next_run_at ASC
             LIMIT ?"
        )
        .bind(&now_str)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn list_upcoming_scheduled_tasks(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<ScheduledTask>> {
        let rows = sqlx::query_as::<_, ScheduledTaskRow>(
            "SELECT id, conversation_id, task_type, task_data, schedule_type, schedule_value, status, next_run_at, last_run_at, run_count, max_runs, created_at, updated_at, last_error
             FROM scheduled_tasks
             WHERE status = 'pending'
             ORDER BY next_run_at IS NULL, next_run_at ASC
             LIMIT ?",
        )
        .bind(limit.clamp(1, 20) as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn mark_task_running_if_pending(&self, id: &str) -> anyhow::Result<bool> {
        let now_str = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE scheduled_tasks
             SET status = 'running', updated_at = ?
             WHERE id = ? AND status = 'pending'",
        )
        .bind(&now_str)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_task_next_run(
        &self,
        id: &str,
        next_run: &str,
        increment_count: bool,
    ) -> anyhow::Result<()> {
        let now_str = Utc::now().to_rfc3339();
        if increment_count {
            sqlx::query(
                "UPDATE scheduled_tasks
                 SET status = 'pending', next_run_at = ?, last_run_at = ?, run_count = run_count + 1, updated_at = ?
                 WHERE id = ?"
            )
            .bind(next_run)
            .bind(&now_str)
            .bind(&now_str)
            .bind(id)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query(
                "UPDATE scheduled_tasks SET status = 'pending', next_run_at = ?, updated_at = ? WHERE id = ?",
            )
                .bind(next_run)
                .bind(&now_str)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn mark_task_completed(&self, id: &str) -> anyhow::Result<()> {
        let now_str = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE scheduled_tasks SET status = 'completed', updated_at = ?, last_run_at = ? WHERE id = ?"
        )
        .bind(&now_str)
        .bind(&now_str)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_task_failed(&self, id: &str, error: &str) -> anyhow::Result<()> {
        let now_str = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE scheduled_tasks SET status = 'failed', last_error = ?, updated_at = ? WHERE id = ?"
        )
        .bind(error)
        .bind(&now_str)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_running_scheduled_tasks_interrupted(&self) -> anyhow::Result<u64> {
        let now = Utc::now().to_rfc3339();
        Ok(sqlx::query(
            "UPDATE scheduled_tasks SET status = 'failed', last_error = 'Interrupted by server restart; not retried automatically', updated_at = ? WHERE status = 'running'",
        )
        .bind(&now)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    pub async fn cancel_scheduled_task(&self, id: &str) -> anyhow::Result<()> {
        let now_str = Utc::now().to_rfc3339();
        sqlx::query("UPDATE scheduled_tasks SET status = 'cancelled', updated_at = ? WHERE id = ?")
            .bind(&now_str)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_scheduled_tasks_for_conversation(
        &self,
        conversation_id: Uuid,
    ) -> anyhow::Result<Vec<ScheduledTask>> {
        let conv_str = conversation_id.to_string();
        let rows = sqlx::query_as::<_, ScheduledTaskRow>(
            "SELECT id, conversation_id, task_type, task_data, schedule_type, schedule_value, status, next_run_at, last_run_at, run_count, max_runs, created_at, updated_at, last_error
             FROM scheduled_tasks
             WHERE conversation_id = ? AND status IN ('pending', 'running')
             ORDER BY next_run_at ASC"
        )
        .bind(&conv_str)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    // Conversation Summaries

    pub async fn get_conversation_summary(
        &self,
        conversation_id: Uuid,
    ) -> anyhow::Result<Option<ConversationSummary>> {
        let conv_str = conversation_id.to_string();
        let row = sqlx::query_as::<_, ConversationSummaryRow>(
            "SELECT conversation_id, summary, messages_summarized, last_message_id, created_at FROM conversation_summaries WHERE conversation_id = ?",
        )
        .bind(&conv_str)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn save_conversation_summary(
        &self,
        conversation_id: Uuid,
        summary: &str,
        messages_summarized: i64,
        last_message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let conv_str = conversation_id.to_string();
        let now_str = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR REPLACE INTO conversation_summaries (conversation_id, summary, messages_summarized, last_message_id, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&conv_str)
        .bind(summary)
        .bind(messages_summarized)
        .bind(last_message_id)
        .bind(&now_str)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_messages_after_for_summary(
        &self,
        conversation_id: Uuid,
        last_message_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<Message>> {
        let rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, conversation_id, role, content, tool_calls, tool_call_id, name, created_at
             FROM messages
             WHERE conversation_id = ?
               AND rowid > COALESCE((SELECT rowid FROM messages WHERE id = ?), 0)
             ORDER BY rowid ASC
             LIMIT ?",
        )
        .bind(conversation_id.to_string())
        .bind(last_message_id)
        .bind(limit.clamp(1, 500) as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(Message::try_from).collect()
    }

    // Out-of-Band Messages

    pub async fn queue_oob_message(
        &self,
        conversation_id: Uuid,
        content: &str,
        priority: &str,
    ) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let conv_str = conversation_id.to_string();
        let now_str = Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO out_of_band_messages (id, conversation_id, content, priority, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&conv_str)
        .bind(content)
        .bind(priority)
        .bind(&now_str)
        .execute(&self.pool)
        .await?;

        Ok(id)
    }

    pub async fn list_pending_oob_messages(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<OutOfBandMessage>> {
        let rows = sqlx::query_as::<_, OutOfBandMessageRow>(
            "SELECT id, conversation_id, sender, content, priority, status, created_at, sent_at, last_error
             FROM out_of_band_messages
             WHERE status = 'pending'
             ORDER BY
               CASE priority
                 WHEN 'urgent' THEN 3
                 WHEN 'high' THEN 2
                 WHEN 'normal' THEN 1
                 WHEN 'low' THEN 0
                 ELSE 0
               END DESC,
               created_at ASC
             LIMIT ?"
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn mark_oob_message_sent(&self, id: &str) -> anyhow::Result<()> {
        let now_str = Utc::now().to_rfc3339();
        sqlx::query("UPDATE out_of_band_messages SET status = 'sent', sent_at = ? WHERE id = ?")
            .bind(&now_str)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_oob_message_failed(&self, id: &str, error: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE out_of_band_messages SET status = 'failed', last_error = ? WHERE id = ?",
        )
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
