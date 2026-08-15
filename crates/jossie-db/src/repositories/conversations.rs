use super::*;

impl Database {
    // Conversations
    pub async fn create_conversation(&self, title: Option<&str>) -> anyhow::Result<Conversation> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let id_str = id.to_string();
        let now_str = now.to_rfc3339();
        sqlx::query(
            "INSERT INTO conversations (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&id_str)
        .bind(title)
        .bind(&now_str)
        .bind(&now_str)
        .execute(&self.pool)
        .await?;
        Ok(Conversation {
            id,
            title: title.map(String::from),
            archived_at: None,
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn get_conversation(&self, id: Uuid) -> anyhow::Result<Option<Conversation>> {
        let id_str = id.to_string();
        let row = sqlx::query_as::<_, ConversationRow>(
            "SELECT id, title, archived_at, created_at, updated_at FROM conversations WHERE id = ?",
        )
        .bind(&id_str)
        .fetch_optional(&self.pool)
        .await?;
        row.map(Conversation::try_from).transpose()
    }

    pub async fn list_conversations(&self) -> anyhow::Result<Vec<Conversation>> {
        let rows = sqlx::query_as::<_, ConversationRow>(
            "SELECT id, title, archived_at, created_at, updated_at FROM conversations WHERE archived_at IS NULL ORDER BY updated_at DESC, id DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(Conversation::try_from).collect()
    }

    pub async fn list_conversation_items(
        &self,
        view: &str,
        query: Option<&str>,
        limit: usize,
        before: Option<Uuid>,
    ) -> anyhow::Result<Vec<ConversationListItem>> {
        let query = query.unwrap_or_default().trim();
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let before = before.map(|id| id.to_string());
        let rows = sqlx::query_as::<_, ConversationListRow>(
            "SELECT c.id, c.title, c.archived_at, c.created_at, c.updated_at,
                    COALESCE(
                      (SELECT substr(m.content, 1, 240) FROM messages m
                       WHERE m.conversation_id = c.id
                         AND m.role IN ('user', 'assistant') AND trim(m.content) != ''
                         AND (? = '' OR lower(m.content) LIKE lower(?) ESCAPE '\\')
                       ORDER BY m.rowid DESC LIMIT 1),
                      c.title
                    ) AS preview,
                    CASE WHEN ? = '' THEN NULL ELSE
                      (SELECT m.id FROM messages m
                       WHERE m.conversation_id = c.id
                         AND m.role IN ('user', 'assistant') AND trim(m.content) != ''
                         AND lower(m.content) LIKE lower(?) ESCAPE '\\'
                       ORDER BY m.rowid DESC LIMIT 1)
                    END AS matched_message_id,
                    (SELECT count(*) FROM messages mc
                     WHERE mc.conversation_id = c.id AND mc.role IN ('user', 'assistant')) AS message_count
             FROM conversations c
             WHERE (? = 'all'
                    OR (? = 'archived' AND c.archived_at IS NOT NULL)
                    OR (? = 'active' AND c.archived_at IS NULL))
               AND (? = '' OR lower(COALESCE(c.title, '')) LIKE lower(?) ESCAPE '\\'
                    OR EXISTS (SELECT 1 FROM messages sm
                               WHERE sm.conversation_id = c.id
                                 AND sm.role IN ('user', 'assistant')
                                 AND lower(sm.content) LIKE lower(?) ESCAPE '\\'))
               AND (? IS NULL OR c.updated_at <
                      (SELECT updated_at FROM conversations WHERE id = ?)
                    OR (c.updated_at = (SELECT updated_at FROM conversations WHERE id = ?)
                        AND c.id < ?))
             ORDER BY c.updated_at DESC, c.id DESC
             LIMIT ?",
        )
        .bind(query)
        .bind(&pattern)
        .bind(query)
        .bind(&pattern)
        .bind(view)
        .bind(view)
        .bind(view)
        .bind(query)
        .bind(&pattern)
        .bind(&pattern)
        .bind(&before)
        .bind(&before)
        .bind(&before)
        .bind(&before)
        .bind(limit.clamp(1, 100) as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(ConversationListItem::try_from)
            .collect()
    }

    pub async fn update_conversation(
        &self,
        id: Uuid,
        title: Option<&str>,
        archived: Option<bool>,
    ) -> anyhow::Result<Option<Conversation>> {
        let Some(_) = self.get_conversation(id).await? else {
            return Ok(None);
        };
        let now = Utc::now().to_rfc3339();
        if let Some(title) = title {
            sqlx::query("UPDATE conversations SET title = ?, updated_at = ? WHERE id = ?")
                .bind(title)
                .bind(&now)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?;
        }
        if let Some(archived) = archived {
            sqlx::query("UPDATE conversations SET archived_at = ?, updated_at = ? WHERE id = ?")
                .bind(archived.then_some(now.clone()))
                .bind(&now)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?;
        }
        self.get_conversation(id).await
    }

    pub async fn conversation_has_active_dependencies(&self, id: Uuid) -> anyhow::Result<bool> {
        let id = id.to_string();
        let active = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
               SELECT 1 FROM work_runs WHERE conversation_id = ? AND status IN ('queued','running','waiting_for_approval')
               UNION ALL SELECT 1 FROM pending_actions WHERE conversation_id = ? AND status IN ('pending','executing')
               UNION ALL SELECT 1 FROM scheduled_tasks WHERE conversation_id = ? AND status IN ('pending','running')
               UNION ALL SELECT 1 FROM goals WHERE conversation_id = ? AND archived_at IS NULL AND status IN ('active','paused','blocked')
             )",
        )
        .bind(&id)
        .bind(&id)
        .bind(&id)
        .bind(&id)
        .fetch_one(&self.pool)
        .await?;
        Ok(active != 0)
    }

    pub async fn conversation_delete_files(
        &self,
        id: Uuid,
    ) -> anyhow::Result<Vec<ConversationDeleteFile>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: String,
            path: String,
        }
        let rows = sqlx::query_as::<_, Row>(
            "SELECT DISTINCT f.id, f.path FROM files f
             WHERE NOT EXISTS (SELECT 1 FROM chat_imports ci WHERE ci.file_id = f.id)
               AND (f.conversation_id = ? OR EXISTS (
                 SELECT 1 FROM message_attachments ma JOIN messages m ON m.id = ma.message_id
                 WHERE ma.file_id = f.id AND m.conversation_id = ?))
               AND NOT EXISTS (
                 SELECT 1 FROM message_attachments ma2 JOIN messages m2 ON m2.id = ma2.message_id
                 WHERE ma2.file_id = f.id AND m2.conversation_id != ?)",
        )
        .bind(id.to_string())
        .bind(id.to_string())
        .bind(id.to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(ConversationDeleteFile {
                    id: row.id.parse().ok()?,
                    path: row.path,
                })
            })
            .collect())
    }

    pub async fn delete_conversation_data(
        &self,
        id: Uuid,
        delete_file_ids: &[Uuid],
    ) -> anyhow::Result<Option<Vec<Uuid>>> {
        let id = id.to_string();
        let mut tx = self.pool.begin().await?;
        let deletable = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
               SELECT 1 FROM conversations c
               WHERE c.id = ? AND c.archived_at IS NOT NULL
                 AND NOT EXISTS (SELECT 1 FROM work_runs WHERE conversation_id = c.id AND status IN ('queued','running','waiting_for_approval'))
                 AND NOT EXISTS (SELECT 1 FROM pending_actions WHERE conversation_id = c.id AND status IN ('pending','executing'))
                 AND NOT EXISTS (SELECT 1 FROM scheduled_tasks WHERE conversation_id = c.id AND status IN ('pending','running'))
                 AND NOT EXISTS (SELECT 1 FROM goals WHERE conversation_id = c.id AND archived_at IS NULL AND status IN ('active','paused','blocked'))
             )",
        )
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?
            != 0;
        if !deletable {
            tx.rollback().await?;
            return Ok(None);
        }
        sqlx::query("DELETE FROM activity_events WHERE conversation_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM work_runs WHERE conversation_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM goals WHERE conversation_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        let mut deleted_file_ids = Vec::new();
        for file_id in delete_file_ids {
            let deleted = sqlx::query_scalar::<_, String>(
                "DELETE FROM files
                 WHERE id = ?
                   AND NOT EXISTS (SELECT 1 FROM chat_imports WHERE file_id = ?)
                   AND NOT EXISTS (
                     SELECT 1 FROM message_attachments ma JOIN messages m ON m.id = ma.message_id
                     WHERE ma.file_id = ? AND m.conversation_id != ?)
                 RETURNING id",
            )
            .bind(file_id.to_string())
            .bind(file_id.to_string())
            .bind(file_id.to_string())
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
            if deleted.is_some() {
                deleted_file_ids.push(*file_id);
            }
        }
        sqlx::query("DELETE FROM messages WHERE conversation_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        let deleted = sqlx::query("DELETE FROM conversations WHERE id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            == 1;
        tx.commit().await?;
        Ok(deleted.then_some(deleted_file_ids))
    }

    pub async fn get_latest_conversation_id(&self) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query_as::<_, ConversationIdRow>(
            "SELECT id FROM conversations ORDER BY archived_at IS NOT NULL, updated_at DESC, id DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.id.parse().ok()))
    }

    // Messages
    pub async fn save_message(&self, msg: &Message) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;

        let id_str = msg.id.to_string();
        let conv_str = msg.conversation_id.to_string();
        let role_str = msg.role.to_string();
        let tc = msg.tool_calls.as_ref().map(|v| v.to_string());
        let created = msg.created_at.to_rfc3339();
        sqlx::query("INSERT INTO messages (id, conversation_id, role, content, tool_calls, tool_call_id, name, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&id_str)
            .bind(&conv_str)
            .bind(&role_str)
            .bind(&msg.content)
            .bind(&tc)
            .bind(&msg.tool_call_id)
            .bind(&msg.name)
            .bind(&created)
            .execute(&mut *tx)
            .await?;

        sqlx::query("UPDATE conversations SET updated_at = ?, archived_at = CASE WHEN ? THEN NULL ELSE archived_at END WHERE id = ?")
            .bind(&created)
            .bind(matches!(msg.role, Role::User | Role::Assistant) && !msg.content.trim().is_empty())
            .bind(&conv_str)
            .execute(&mut *tx)
            .await?;

        if msg.role == Role::User
            && msg.name.is_none()
            && let Some(title) = Self::conversation_title_from_content(&msg.content)
        {
            sqlx::query(
                "UPDATE conversations
                     SET title = CASE
                         WHEN title IS NULL OR trim(title) = '' THEN ?
                         ELSE title
                     END
                     WHERE id = ?",
            )
            .bind(title)
            .bind(&conv_str)
            .execute(&mut *tx)
            .await?;
        }

        if let Some(attachments) = &msg.attachments {
            for attachment in attachments {
                sqlx::query(
                    "INSERT OR IGNORE INTO message_attachments (message_id, file_id) VALUES (?, ?)",
                )
                .bind(&id_str)
                .bind(attachment.id.to_string())
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn get_messages(
        &self,
        conversation_id: Uuid,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<Message>> {
        let conv_str = conversation_id.to_string();

        let rows = if let Some(limit) = limit {
            let limit_val = limit.clamp(1, 200) as i64;
            let mut rows = sqlx::query_as::<_, MessageRow>("SELECT id, conversation_id, role, content, tool_calls, tool_call_id, name, created_at FROM messages WHERE conversation_id = ? ORDER BY created_at DESC LIMIT ?")
                .bind(&conv_str)
                .bind(limit_val)
                .fetch_all(&self.pool)
                .await?;
            // Reverse to get chronological order (oldest first)
            rows.reverse();
            rows
        } else {
            sqlx::query_as::<_, MessageRow>("SELECT id, conversation_id, role, content, tool_calls, tool_call_id, name, created_at FROM messages WHERE conversation_id = ? ORDER BY created_at ASC")
                .bind(&conv_str)
                .fetch_all(&self.pool)
                .await?
        };

        self.hydrate_message_rows(rows).await
    }

    pub async fn get_messages_before(
        &self,
        conversation_id: Uuid,
        before: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<Message>> {
        let mut rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, conversation_id, role, content, tool_calls, tool_call_id, name, created_at
             FROM messages WHERE conversation_id = ?
               AND rowid < COALESCE((SELECT rowid FROM messages WHERE id = ? AND conversation_id = ?), 0)
             ORDER BY rowid DESC LIMIT ?",
        )
        .bind(conversation_id.to_string())
        .bind(before.to_string())
        .bind(conversation_id.to_string())
        .bind(limit.clamp(1, 200) as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        self.hydrate_message_rows(rows).await
    }

    pub async fn get_messages_around(
        &self,
        conversation_id: Uuid,
        around: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<Message>> {
        let conv = conversation_id.to_string();
        let around = around.to_string();
        let half = (limit.clamp(1, 200) / 2) as i64;
        let mut before = sqlx::query_as::<_, MessageRow>(
            "SELECT id, conversation_id, role, content, tool_calls, tool_call_id, name, created_at
             FROM messages WHERE conversation_id = ? AND rowid <=
               COALESCE((SELECT rowid FROM messages WHERE id = ? AND conversation_id = ?), 0)
             ORDER BY rowid DESC LIMIT ?",
        )
        .bind(&conv)
        .bind(&around)
        .bind(&conv)
        .bind(half + 1)
        .fetch_all(&self.pool)
        .await?;
        before.reverse();
        let remaining = limit.clamp(1, 200).saturating_sub(before.len()) as i64;
        let after = sqlx::query_as::<_, MessageRow>(
            "SELECT id, conversation_id, role, content, tool_calls, tool_call_id, name, created_at
             FROM messages WHERE conversation_id = ? AND rowid >
               COALESCE((SELECT rowid FROM messages WHERE id = ? AND conversation_id = ?), 0)
             ORDER BY rowid ASC LIMIT ?",
        )
        .bind(&conv)
        .bind(&around)
        .bind(&conv)
        .bind(remaining)
        .fetch_all(&self.pool)
        .await?;
        before.extend(after);
        self.hydrate_message_rows(before).await
    }

    pub async fn get_message(&self, id: Uuid) -> anyhow::Result<Option<Message>> {
        let rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, conversation_id, role, content, tool_calls, tool_call_id, name, created_at FROM messages WHERE id = ?",
        ).bind(id.to_string()).fetch_all(&self.pool).await?;
        Ok(self.hydrate_message_rows(rows).await?.into_iter().next())
    }

    async fn hydrate_message_rows(&self, rows: Vec<MessageRow>) -> anyhow::Result<Vec<Message>> {
        let message_ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        let mut attachments_by_message = self.get_message_attachments_many(&message_ids).await?;

        let mut messages = Vec::new();
        for row in rows {
            let mut msg = Message::try_from(row)?;
            let attachments = attachments_by_message.remove(&msg.id).unwrap_or_default();
            if !attachments.is_empty() {
                msg.attachments = Some(
                    attachments
                        .into_iter()
                        .map(|f| jossie_core::types::Attachment {
                            id: f.id,
                            name: f.name,
                            mime_type: f.mime_type,
                            size: f.size,
                            data: None,
                        })
                        .collect(),
                );
            }
            messages.push(msg);
        }
        Ok(messages)
    }

    async fn get_message_attachments_many(
        &self,
        message_ids: &[String],
    ) -> anyhow::Result<HashMap<Uuid, Vec<FileRecord>>> {
        if message_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT ma.message_id, f.id, f.name, f.mime_type, f.size, f.path,
                    f.conversation_id, f.created_at
             FROM message_attachments ma
             INNER JOIN files f ON f.id = ma.file_id
             WHERE ma.message_id IN (",
        );
        {
            let mut separated = builder.separated(", ");
            for message_id in message_ids {
                separated.push_bind(message_id);
            }
        }
        builder.push(") ORDER BY f.created_at ASC");
        let rows = builder
            .build_query_as::<MessageAttachmentFileRow>()
            .fetch_all(&self.pool)
            .await?;

        let mut by_message: HashMap<Uuid, Vec<FileRecord>> = HashMap::new();
        for row in rows {
            let message_id = row
                .message_id
                .parse::<Uuid>()
                .context("invalid attachment message id")?;
            by_message
                .entry(message_id)
                .or_default()
                .push(FileRecord::try_from(row)?);
        }
        Ok(by_message)
    }

    // Memory (FTS5)
}
