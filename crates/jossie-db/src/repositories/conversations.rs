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
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn get_conversation(&self, id: Uuid) -> anyhow::Result<Option<Conversation>> {
        let id_str = id.to_string();
        let row = sqlx::query_as::<_, ConversationRow>(
            "SELECT id, title, created_at, updated_at FROM conversations WHERE id = ?",
        )
        .bind(&id_str)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into()))
    }

    pub async fn list_conversations(&self) -> anyhow::Result<Vec<Conversation>> {
        let rows = sqlx::query_as::<_, ConversationRow>(
            "SELECT id, title, created_at, updated_at FROM conversations ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn get_latest_conversation_id(&self) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query_as::<_, ConversationIdRow>(
            "SELECT id FROM conversations ORDER BY updated_at DESC LIMIT 1",
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

        sqlx::query("UPDATE conversations SET updated_at = ? WHERE id = ?")
            .bind(&created)
            .bind(&conv_str)
            .execute(&mut *tx)
            .await?;

        if msg.role == Role::User && msg.name.is_none() {
            if let Some(title) = Self::conversation_title_from_content(&msg.content) {
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
            let limit_val = limit as i64;
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

        let message_ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        let mut attachments_by_message = self.get_message_attachments_many(&message_ids).await?;

        let mut messages = Vec::new();
        for row in rows {
            let mut msg: Message = row.into();
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
            let Ok(message_id) = row.message_id.parse::<Uuid>() else {
                continue;
            };
            by_message.entry(message_id).or_default().push(row.into());
        }
        Ok(by_message)
    }

    // Memory (FTS5)
}
