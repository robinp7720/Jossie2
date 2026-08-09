use super::*;

impl Database {
    // Files

    pub async fn save_file_record(
        &self,
        id: &Uuid,
        name: &str,
        mime_type: Option<&str>,
        size: i64,
        path: &str,
        conversation_id: Option<Uuid>,
    ) -> anyhow::Result<()> {
        let id_str = id.to_string();
        let conv_str = conversation_id.map(|u| u.to_string());
        sqlx::query(
            "INSERT INTO files (id, name, mime_type, size, path, conversation_id) VALUES (?, ?, ?, ?, ?, ?)"
        )
        .bind(&id_str)
        .bind(name)
        .bind(mime_type)
        .bind(size)
        .bind(path)
        .bind(conv_str)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_file_record(&self, id: &Uuid) -> anyhow::Result<Option<FileRecord>> {
        let id_str = id.to_string();
        let row = sqlx::query_as::<_, FileRow>("SELECT * FROM files WHERE id = ?")
            .bind(&id_str)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(Into::into))
    }

    pub async fn delete_file_record(&self, id: &Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM files WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_files_for_conversation(
        &self,
        conversation_id: Uuid,
    ) -> anyhow::Result<Vec<FileRecord>> {
        let conv_str = conversation_id.to_string();
        let rows = sqlx::query_as::<_, FileRow>(
            "SELECT * FROM files WHERE conversation_id = ? ORDER BY created_at DESC",
        )
        .bind(&conv_str)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn link_message_attachment(
        &self,
        message_id: Uuid,
        file_id: Uuid,
    ) -> anyhow::Result<()> {
        let msg_str = message_id.to_string();
        let file_str = file_id.to_string();
        sqlx::query(
            "INSERT OR IGNORE INTO message_attachments (message_id, file_id) VALUES (?, ?)",
        )
        .bind(&msg_str)
        .bind(&file_str)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_message_attachments(
        &self,
        message_id: Uuid,
    ) -> anyhow::Result<Vec<FileRecord>> {
        let msg_str = message_id.to_string();
        let rows = sqlx::query_as::<_, FileRow>(
            "SELECT f.* FROM files f
             JOIN message_attachments ma ON f.id = ma.file_id
             WHERE ma.message_id = ?
             ORDER BY f.created_at ASC",
        )
        .bind(&msg_str)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn create_chat_import(
        &self,
        file_id: Uuid,
        format: &str,
    ) -> anyhow::Result<ChatImport> {
        if let Some(existing) = self.get_chat_import_by_file(file_id).await? {
            if existing.status == "failed" && existing.format != format {
                sqlx::query(
                    "UPDATE chat_imports SET format = ?, updated_at = ?
                     WHERE id = ? AND status = 'failed'",
                )
                .bind(format)
                .bind(Utc::now().to_rfc3339())
                .bind(&existing.id)
                .execute(&self.pool)
                .await?;
                return self
                    .get_chat_import(&existing.id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Chat import could not be reloaded"));
            }
            return Ok(existing);
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR IGNORE INTO chat_imports (id, file_id, format, status, created_at, updated_at)
             VALUES (?, ?, ?, 'queued', ?, ?)",
        )
        .bind(&id)
        .bind(file_id.to_string())
        .bind(format)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.get_chat_import_by_file(file_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Created chat import could not be loaded"))
    }

    pub async fn get_chat_import(&self, id: &str) -> anyhow::Result<Option<ChatImport>> {
        Ok(sqlx::query_as::<_, ChatImport>(
            "SELECT id, file_id, format, status, total_messages, analyzed_messages,
                    memories_saved, nodes_saved, edges_saved, error, created_at, updated_at
             FROM chat_imports WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn get_chat_import_by_file(
        &self,
        file_id: Uuid,
    ) -> anyhow::Result<Option<ChatImport>> {
        Ok(sqlx::query_as::<_, ChatImport>(
            "SELECT id, file_id, format, status, total_messages, analyzed_messages,
                    memories_saved, nodes_saved, edges_saved, error, created_at, updated_at
             FROM chat_imports WHERE file_id = ?",
        )
        .bind(file_id.to_string())
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn claim_chat_import(&self, id: &str) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE chat_imports SET status = 'processing', error = NULL, updated_at = ?
             WHERE id = ? AND status IN ('queued', 'failed')",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn complete_chat_import(
        &self,
        id: &str,
        format: &str,
        total_messages: usize,
        analyzed_messages: usize,
        memories_saved: usize,
        nodes_saved: usize,
        edges_saved: usize,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE chat_imports
             SET format = ?, status = 'completed', total_messages = ?, analyzed_messages = ?,
                 memories_saved = ?, nodes_saved = ?, edges_saved = ?, error = NULL, updated_at = ?
             WHERE id = ?",
        )
        .bind(format)
        .bind(total_messages as i64)
        .bind(analyzed_messages as i64)
        .bind(memories_saved as i64)
        .bind(nodes_saved as i64)
        .bind(edges_saved as i64)
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_chat_import_progress(
        &self,
        id: &str,
        format: &str,
        total_messages: usize,
        analyzed_messages: usize,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE chat_imports SET format = ?, total_messages = ?, analyzed_messages = ?,
                    updated_at = ? WHERE id = ? AND status = 'processing'",
        )
        .bind(format)
        .bind(total_messages as i64)
        .bind(analyzed_messages as i64)
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn fail_chat_import(&self, id: &str, error: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE chat_imports SET status = 'failed', error = ?, updated_at = ? WHERE id = ?",
        )
        .bind(error)
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn requeue_interrupted_chat_imports(&self) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "UPDATE chat_imports SET status = 'queued',
                    error = 'The server restarted while this import was running; it was queued again.',
                    updated_at = ?
             WHERE status = 'processing'",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn list_queued_chat_imports(&self) -> anyhow::Result<Vec<ChatImport>> {
        Ok(sqlx::query_as::<_, ChatImport>(
            "SELECT id, file_id, format, status, total_messages, analyzed_messages,
                    memories_saved, nodes_saved, edges_saved, error, created_at, updated_at
             FROM chat_imports WHERE status = 'queued' ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn list_recent_chat_imports(&self, limit: usize) -> anyhow::Result<Vec<ChatImport>> {
        Ok(sqlx::query_as::<_, ChatImport>(
            "SELECT id, file_id, format, status, total_messages, analyzed_messages,
                    memories_saved, nodes_saved, edges_saved, error, created_at, updated_at
             FROM chat_imports ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 100) as i64)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn create_pending_action(
        &self,
        action: &NewPendingAction,
    ) -> anyhow::Result<PendingAction> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO pending_actions
             (id, batch_id, conversation_id, run_id, call_id, tool_name, arguments, title, summary, effect, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(&id)
        .bind(&action.batch_id)
        .bind(action.conversation_id.to_string())
        .bind(&action.run_id)
        .bind(&action.call_id)
        .bind(&action.tool_name)
        .bind(&action.arguments)
        .bind(&action.title)
        .bind(&action.summary)
        .bind(&action.effect)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.get_pending_action(&id)
            .await?
            .context("Pending action disappeared after insert")
    }

    pub async fn get_pending_action(&self, id: &str) -> anyhow::Result<Option<PendingAction>> {
        let row = sqlx::query_as::<_, PendingActionRow>(
            "SELECT id, batch_id, conversation_id, run_id, call_id, tool_name, arguments,
                    title, summary, effect, status, result_error, created_at, updated_at, resolved_at
             FROM pending_actions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn list_pending_actions(
        &self,
        conversation_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<PendingAction>> {
        let rows = if let Some(conversation_id) = conversation_id {
            sqlx::query_as::<_, PendingActionRow>(
                "SELECT id, batch_id, conversation_id, run_id, call_id, tool_name, arguments,
                        title, summary, effect, status, result_error, created_at, updated_at, resolved_at
                 FROM pending_actions
                 WHERE conversation_id = ? AND status IN ('pending', 'executing', 'uncertain')
                 ORDER BY created_at ASC",
            )
            .bind(conversation_id.to_string())
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, PendingActionRow>(
                "SELECT id, batch_id, conversation_id, run_id, call_id, tool_name, arguments,
                        title, summary, effect, status, result_error, created_at, updated_at, resolved_at
                 FROM pending_actions
                 WHERE status IN ('pending', 'executing', 'uncertain')
                 ORDER BY created_at ASC",
            )
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn claim_pending_action(&self, id: &str) -> anyhow::Result<Option<PendingAction>> {
        let now = Utc::now().to_rfc3339();
        let changed = sqlx::query(
            "UPDATE pending_actions SET status = 'executing', updated_at = ?
             WHERE id = ? AND status = 'pending'",
        )
        .bind(&now)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 0 {
            return Ok(None);
        }
        self.get_pending_action(id).await
    }

    pub async fn resolve_pending_action(
        &self,
        id: &str,
        status: &str,
        result_error: Option<&str>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(status, "completed" | "failed" | "rejected" | "uncertain"),
            "Invalid pending action terminal status"
        );
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE pending_actions
             SET status = ?, result_error = ?, updated_at = ?, resolved_at = ?
             WHERE id = ?",
        )
        .bind(status)
        .bind(result_error)
        .bind(&now)
        .bind(&now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn pending_action_batch_is_resolved(&self, batch_id: &str) -> anyhow::Result<bool> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pending_actions
             WHERE batch_id = ? AND status IN ('pending', 'executing')",
        )
        .bind(batch_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count == 0)
    }

    pub async fn has_blocking_pending_actions(
        &self,
        conversation_id: Uuid,
    ) -> anyhow::Result<bool> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pending_actions
             WHERE conversation_id = ? AND status IN ('pending', 'executing')",
        )
        .bind(conversation_id.to_string())
        .fetch_one(&self.pool)
        .await?;
        Ok(count > 0)
    }

    pub async fn mark_interrupted_actions_uncertain(&self) -> anyhow::Result<u64> {
        let now = Utc::now().to_rfc3339();
        let warning = "The server stopped while this action was executing. Its outcome is uncertain. Verify the external system before trying again.";
        let interrupted = sqlx::query_as::<_, PendingActionRow>(
            "SELECT id, batch_id, conversation_id, run_id, call_id, tool_name, arguments,
                    title, summary, effect, status, result_error, created_at, updated_at, resolved_at
             FROM pending_actions WHERE status = 'executing'",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut tx = self.pool.begin().await?;
        for action in &interrupted {
            sqlx::query(
                "INSERT INTO messages
                 (id, conversation_id, role, content, tool_call_id, name, created_at)
                 VALUES (?, ?, 'tool', ?, ?, ?, ?)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(&action.conversation_id)
            .bind(warning)
            .bind(&action.call_id)
            .bind(&action.tool_name)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE pending_actions
             SET status = 'uncertain', result_error = 'The server stopped while this action was executing. Verify the external system before trying again.', updated_at = ?, resolved_at = ?
             WHERE status = 'executing'",
        )
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(interrupted.len() as u64)
    }
}
