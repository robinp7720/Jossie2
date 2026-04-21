use anyhow::Context;
use chrono::Utc;
use jossie_core::types::{Conversation, Message, Role};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

pub struct Database {
    pool: SqlitePool,
    url: String,
}

impl Database {
    fn conversation_title_from_content(content: &str) -> Option<String> {
        let single_line = content.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = single_line.trim();
        if trimmed.is_empty() {
            return None;
        }

        let mut title = trimmed.chars().take(72).collect::<String>();
        if trimmed.len() > 72 {
            title.push_str("...");
        }
        Some(title)
    }

    pub async fn new(url: &str) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::from_str(url)?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        Ok(Self {
            pool,
            url: url.to_string(),
        })
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        if self.attachment_schema_needs_repair().await? {
            self.repair_attachment_schema_with_backup(
                "Detected invalid attachment schema entry in sqlite_schema; attempting repair",
            )
            .await?;
        }

        if let Err(error) = self.run_migrations().await {
            if self.is_repairable_attachment_schema_error(&error) {
                self.repair_attachment_schema_with_backup(
                    "Detected malformed attachment schema while migrating SQLite database; attempting repair",
                )
                .await?;
                self.run_migrations().await?;
                return Ok(());
            }

            tracing::error!("Migration failed: {error}");
            return Err(error);
        }

        Ok(())
    }

    async fn repair_attachment_schema_with_backup(&self, message: &str) -> anyhow::Result<()> {
        tracing::warn!("{message}");
        let backup_path = self.backup_database_file().await?;
        if let Some(path) = backup_path.as_ref() {
            tracing::warn!(
                "Created backup before SQLite attachment schema repair: {}",
                path.display()
            );
        }
        self.repair_attachment_schema().await?;
        tracing::info!("SQLite attachment schema repaired successfully");
        Ok(())
    }

    async fn run_migrations(&self) -> anyhow::Result<()> {
        // Split migrations into individual statements and run each one.
        // IF NOT EXISTS clauses handle idempotency; real errors are propagated.
        let sql = include_str!("../../jossie-db/migrations.sql");
        for statement in sql.split(';') {
            let statement = statement.trim();
            if statement.is_empty() {
                continue;
            }
            if let Err(e) = sqlx::query(statement).execute(&self.pool).await {
                // FTS5 virtual tables can't use IF NOT EXISTS, so ignore "already exists" errors
                let msg = e.to_string();
                if msg.contains("already exists") {
                    tracing::debug!(
                        "Migration statement skipped (already exists): {}",
                        &statement[..statement.len().min(80)]
                    );
                } else {
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }

    fn is_repairable_attachment_schema_message(&self, message: &str) -> bool {
        let message = message.to_lowercase();
        if !message.contains("malformed database schema") {
            return false;
        }

        message.contains("(files)")
            || message.contains("(message_attachments)")
            || message.contains("sqlite_autoindex_files_")
            || message.contains("sqlite_autoindex_message_attachments_")
    }

    fn is_repairable_attachment_schema_error(&self, error: &anyhow::Error) -> bool {
        self.is_repairable_attachment_schema_message(&error.to_string())
    }

    async fn attachment_schema_needs_repair(&self) -> anyhow::Result<bool> {
        let rows = match sqlx::query_as::<_, SchemaEntryRow>(
            "SELECT type AS entry_type, name, tbl_name, rootpage
             FROM sqlite_schema
             WHERE name IN ('files', 'message_attachments')
                OR tbl_name IN ('files', 'message_attachments')",
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                if self.is_repairable_attachment_schema_message(&error.to_string()) {
                    return Ok(true);
                }
                return Err(error.into());
            }
        };

        let has_files_table = rows
            .iter()
            .any(|row| row.entry_type == "table" && row.name == "files");
        let has_message_attachments_table = rows
            .iter()
            .any(|row| row.entry_type == "table" && row.name == "message_attachments");
        let page_count = sqlx::query_scalar::<_, i64>("PRAGMA page_count")
            .fetch_one(&self.pool)
            .await?;

        for row in &rows {
            if matches!(row.entry_type.as_str(), "table" | "index")
                && (row.rootpage <= 0 || row.rootpage > page_count)
            {
                return Ok(true);
            }
            if row.tbl_name == "files" && !has_files_table {
                return Ok(true);
            }
            if row.tbl_name == "message_attachments" && !has_message_attachments_table {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn sqlite_file_path(&self) -> Option<PathBuf> {
        let path = self.url.strip_prefix("sqlite:")?;
        let path = path.split_once('?').map_or(path, |(path, _)| path);
        if path == ":memory:" || path.is_empty() {
            return None;
        }

        let normalized = if let Some(path) = path.strip_prefix("///") {
            PathBuf::from(format!("/{path}"))
        } else if let Some(path) = path.strip_prefix("//") {
            PathBuf::from(path)
        } else {
            PathBuf::from(path)
        };

        if normalized.as_os_str().is_empty() {
            return None;
        }

        Some(normalized)
    }

    async fn backup_database_file(&self) -> anyhow::Result<Option<PathBuf>> {
        let Some(path) = self.sqlite_file_path() else {
            return Ok(None);
        };

        if !path.exists() {
            return Ok(None);
        }

        let backup_path = backup_path_for(&path);
        copy_if_exists(&path, &backup_path).with_context(|| {
            format!(
                "Failed to create SQLite backup before schema repair: {} -> {}",
                path.display(),
                backup_path.display()
            )
        })?;

        for suffix in ["-wal", "-shm"] {
            let sidecar = sidecar_path(&path, suffix);
            let sidecar_backup = sidecar_path(&backup_path, suffix);
            copy_if_exists(&sidecar, &sidecar_backup).with_context(|| {
                format!(
                    "Failed to copy SQLite sidecar file before schema repair: {} -> {}",
                    sidecar.display(),
                    sidecar_backup.display()
                )
            })?;
        }

        Ok(Some(backup_path))
    }

    async fn repair_attachment_schema(&self) -> anyhow::Result<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("PRAGMA writable_schema=ON")
            .execute(&mut *conn)
            .await
            .context("Failed to enable writable_schema during SQLite repair")?;

        let delete_result = sqlx::query(
            "DELETE FROM sqlite_schema WHERE name IN ('files', 'message_attachments') OR tbl_name IN ('files', 'message_attachments')",
        )
        .execute(&mut *conn)
        .await;

        sqlx::query("PRAGMA writable_schema=OFF")
            .execute(&mut *conn)
            .await
            .context("Failed to disable writable_schema after SQLite repair")?;

        delete_result.context("Failed to remove malformed attachment schema entries")?;

        sqlx::query("VACUUM")
            .execute(&mut *conn)
            .await
            .context("Failed to VACUUM SQLite database after attachment schema repair")?;

        let integrity_check = sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_all(&mut *conn)
            .await
            .context("Failed to run SQLite integrity_check after attachment schema repair")?;

        anyhow::ensure!(
            integrity_check.len() == 1 && integrity_check[0] == "ok",
            "SQLite integrity_check failed after attachment schema repair: {}",
            integrity_check.join("; ")
        );

        Ok(())
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn health_check(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }

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

        let mut messages = Vec::new();
        for row in rows {
            let mut msg: Message = row.into();
            let attachments = self.get_message_attachments(msg.id).await?;
            if !attachments.is_empty() {
                msg.attachments = Some(
                    attachments
                        .into_iter()
                        .map(|f| jossie_core::types::Attachment {
                            id: f.id,
                            name: f.name,
                            mime_type: f.mime_type,
                            size: f.size,
                        })
                        .collect(),
                );
            }
            messages.push(msg);
        }
        Ok(messages)
    }

    // Memory (FTS5)
    pub async fn memory_save(&self, key: &str, content: &str, tags: &str) -> anyhow::Result<()> {
        // Delete existing entry if any
        sqlx::query("DELETE FROM memory WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await
            .ok();
        sqlx::query("INSERT INTO memory (key, content, tags) VALUES (?, ?, ?)")
            .bind(key)
            .bind(content)
            .bind(tags)
            .execute(&self.pool)
            .await?;
        let now_str = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR REPLACE INTO memory_metadata (key, created_at, updated_at) VALUES (?, ?, ?)",
        )
        .bind(key)
        .bind(&now_str)
        .bind(&now_str)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn memory_search(&self, query: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        const LIMIT: usize = 10;
        const FETCH_PER_STRATEGY: usize = 12;

        let mut results = Vec::new();
        let mut seen_keys = HashSet::new();

        for match_query in build_memory_search_queries(query) {
            let rows = match self
                .memory_search_match(&match_query, FETCH_PER_STRATEGY)
                .await
            {
                Ok(rows) => rows,
                Err(err) => {
                    tracing::warn!(
                        "Memory search strategy failed for query {:?}: {err}",
                        match_query
                    );
                    continue;
                }
            };

            for row in rows {
                if seen_keys.insert(row.key.clone()) {
                    results.push(MemoryEntry {
                        key: row.key,
                        content: row.content,
                        tags: row.tags,
                    });
                    if results.len() >= LIMIT {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    pub async fn get_memory(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let row =
            sqlx::query_as::<_, MemoryRow>("SELECT key, content, tags FROM memory WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| MemoryEntry {
            key: r.key,
            content: r.content,
            tags: r.tags,
        }))
    }

    pub async fn memory_list_keys(&self) -> anyhow::Result<Vec<MemoryKeyInfo>> {
        let rows = sqlx::query_as::<_, MemoryKeyRow>(
            "SELECT m.key, mm.created_at, mm.updated_at 
             FROM memory m 
             LEFT JOIN memory_metadata mm ON m.key = mm.key 
             ORDER BY mm.updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| MemoryKeyInfo {
                key: r.key,
                created_at: r.created_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
                updated_at: r.updated_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
            })
            .collect())
    }

    pub async fn memory_list_all(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntryWithMetadata>> {
        let limit = limit.max(1).min(500);
        let rows = sqlx::query_as::<_, MemoryEntryMetadataRow>(
            "SELECT m.key, m.content, m.tags, mm.created_at, mm.updated_at 
             FROM memory m 
             LEFT JOIN memory_metadata mm ON m.key = mm.key 
             ORDER BY mm.updated_at DESC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| MemoryEntryWithMetadata {
                key: r.key,
                content: r.content,
                tags: r.tags,
                created_at: r.created_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
                updated_at: r.updated_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
            })
            .collect())
    }

    async fn memory_search_match(
        &self,
        match_query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryRow>> {
        let rows = sqlx::query_as::<_, MemoryRow>(
            "SELECT key, content, tags
             FROM memory
             WHERE memory MATCH ?
             ORDER BY bm25(memory, 8.0, 1.0, 3.0), rowid DESC
             LIMIT ?",
        )
        .bind(match_query)
        .bind(limit.max(1).min(100) as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

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

    // Knowledge Graph

    pub async fn graph_upsert_node(
        &self,
        id: &str,
        label: &str,
        node_type: &str,
        properties: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let props_str = serde_json::to_string(properties)?;
        let now_str = Utc::now().to_rfc3339();

        // Use normalized ID if provided, otherwise generate one (but usually ID is derived from label for deduplication)
        // Here we assume caller provides a stable ID (e.g. lowercase label)

        sqlx::query(
            "INSERT INTO graph_nodes (id, label, type, properties, created_at, updated_at) 
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET 
                label = excluded.label, 
                type = excluded.type,
                properties = excluded.properties,
                updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(label)
        .bind(node_type)
        .bind(&props_str)
        .bind(&now_str)
        .bind(&now_str)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn graph_upsert_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relation: &str,
        weight: f64,
        properties: &serde_json::Value,
    ) -> anyhow::Result<String> {
        // Check if edge exists with same source, target, relation
        // We'll treat (source, target, relation) as unique for simplicity in this iteration,
        // though the DB schema uses a UUID PK.

        let props_str = serde_json::to_string(properties)?;
        let now_str = Utc::now().to_rfc3339();

        let existing = sqlx::query_as::<_, GraphEdgeRow>(
            "SELECT * FROM graph_edges WHERE source_id = ? AND target_id = ? AND relation = ?",
        )
        .bind(source_id)
        .bind(target_id)
        .bind(relation)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(edge) = existing {
            sqlx::query(
                "UPDATE graph_edges SET weight = ?, properties = ?, updated_at = ? WHERE id = ?",
            )
            .bind(weight)
            .bind(&props_str)
            .bind(&now_str)
            .bind(&edge.id)
            .execute(&self.pool)
            .await?;
            Ok(edge.id)
        } else {
            let id = Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO graph_edges (id, source_id, target_id, relation, weight, properties, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(&id)
            .bind(source_id)
            .bind(target_id)
            .bind(relation)
            .bind(weight)
            .bind(&props_str)
            .bind(&now_str)
            .bind(&now_str)
            .execute(&self.pool)
            .await?;
            Ok(id)
        }
    }

    pub async fn graph_get_node(&self, id: &str) -> anyhow::Result<Option<GraphNode>> {
        let row = sqlx::query_as::<_, GraphNodeRow>("SELECT * FROM graph_nodes WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(Into::into))
    }

    pub async fn graph_find_nodes(&self, query: &str) -> anyhow::Result<Vec<GraphNode>> {
        let search = format!("%{}%", query);
        let rows = sqlx::query_as::<_, GraphNodeRow>(
            "SELECT * FROM graph_nodes WHERE label LIKE ? LIMIT 20",
        )
        .bind(search)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn graph_get_neighbors(&self, node_id: &str) -> anyhow::Result<Vec<GraphNeighbor>> {
        // Outgoing edges
        let outgoing = sqlx::query_as::<_, GraphNeighborRow>(
            r#"
            SELECT e.id as edge_id, e.relation, e.weight, 
                   n.id as node_id, n.label, n.type as node_type, n.properties as node_properties
            FROM graph_edges e
            JOIN graph_nodes n ON e.target_id = n.id
            WHERE e.source_id = ?
            "#,
        )
        .bind(node_id)
        .fetch_all(&self.pool)
        .await?;

        // Incoming edges
        let incoming = sqlx::query_as::<_, GraphNeighborRow>(
            r#"
            SELECT e.id as edge_id, e.relation, e.weight, 
                   n.id as node_id, n.label, n.type as node_type, n.properties as node_properties
            FROM graph_edges e
            JOIN graph_nodes n ON e.source_id = n.id
            WHERE e.target_id = ?
            "#,
        )
        .bind(node_id)
        .fetch_all(&self.pool)
        .await?;

        let mut results = Vec::new();
        for r in outgoing {
            results.push(GraphNeighbor {
                edge_id: r.edge_id,
                relation: r.relation,
                direction: "outgoing".to_string(),
                node: GraphNode {
                    id: r.node_id,
                    label: r.label,
                    node_type: r.node_type,
                    properties: serde_json::from_str(&r.node_properties).unwrap_or_else(|e| {
                        tracing::warn!("Failed to parse graph node properties: {e}");
                        serde_json::Value::default()
                    }),
                },
            });
        }
        for r in incoming {
            results.push(GraphNeighbor {
                edge_id: r.edge_id,
                relation: r.relation,
                direction: "incoming".to_string(),
                node: GraphNode {
                    id: r.node_id,
                    label: r.label,
                    node_type: r.node_type,
                    properties: serde_json::from_str(&r.node_properties).unwrap_or_else(|e| {
                        tracing::warn!("Failed to parse graph node properties: {e}");
                        serde_json::Value::default()
                    }),
                },
            });
        }

        Ok(results)
    }

    pub async fn graph_list_nodes(&self, limit: usize) -> anyhow::Result<Vec<GraphNode>> {
        let limit = limit.max(1).min(5000);
        let rows = sqlx::query_as::<_, GraphNodeRow>(
            "SELECT * FROM graph_nodes ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn graph_list_edges(&self, limit: usize) -> anyhow::Result<Vec<GraphEdge>> {
        let limit = limit.max(1).min(5000);
        let rows = sqlx::query_as::<_, GraphEdgeRow>(
            "SELECT * FROM graph_edges ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get all nodes of a specific type (e.g., "Person", "Project", "Company")
    pub async fn graph_list_nodes_by_type(
        &self,
        node_type: &str,
    ) -> anyhow::Result<Vec<GraphNode>> {
        let rows = sqlx::query_as::<_, GraphNodeRow>(
            "SELECT * FROM graph_nodes WHERE type = ? ORDER BY updated_at DESC LIMIT 50",
        )
        .bind(node_type)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get the most recently updated nodes (for fresh context)
    pub async fn graph_recent_nodes(&self, limit: usize) -> anyhow::Result<Vec<GraphNode>> {
        let limit = limit.max(1).min(100);
        let rows = sqlx::query_as::<_, GraphNodeRow>(
            "SELECT * FROM graph_nodes ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get nodes with the most connections (important/central entities)
    pub async fn graph_central_nodes(&self, limit: usize) -> anyhow::Result<Vec<(GraphNode, i64)>> {
        let limit = limit.max(1).min(50);

        #[derive(sqlx::FromRow)]
        struct CentralNodeRow {
            id: String,
            label: String,
            #[sqlx(rename = "type")]
            node_type: String,
            properties: String,
            #[allow(dead_code)]
            created_at: String,
            #[allow(dead_code)]
            updated_at: String,
            connection_count: i64,
        }

        let rows = sqlx::query_as::<_, CentralNodeRow>(
            r#"
            SELECT n.*, COUNT(e.id) as connection_count
            FROM graph_nodes n
            LEFT JOIN graph_edges e ON e.source_id = n.id OR e.target_id = n.id
            GROUP BY n.id
            ORDER BY connection_count DESC
            LIMIT ?
            "#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let node = GraphNode {
                    id: r.id,
                    label: r.label,
                    node_type: r.node_type,
                    properties: serde_json::from_str(&r.properties).unwrap_or_else(|e| {
                        tracing::warn!("Failed to parse graph node properties: {e}");
                        serde_json::Value::default()
                    }),
                };
                (node, r.connection_count)
            })
            .collect())
    }

    // Scheduled Tasks

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
}

fn backup_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("jossie.db");
    let backup_name = format!(
        "{file_name}.repair-backup-{}-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        Uuid::new_v4()
    );
    path.with_file_name(backup_name)
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn copy_if_exists(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.exists() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    Ok(())
}

// Row types for sqlx
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileRecord {
    pub id: Uuid,
    pub name: String,
    pub mime_type: Option<String>,
    pub size: i64,
    pub path: String,
    pub conversation_id: Option<Uuid>,
    pub created_at: String,
}

#[derive(sqlx::FromRow)]
struct FileRow {
    id: String,
    name: String,
    mime_type: Option<String>,
    size: i64,
    path: String,
    conversation_id: Option<String>,
    created_at: String,
}

impl From<FileRow> for FileRecord {
    fn from(r: FileRow) -> Self {
        FileRecord {
            id: r.id.parse().unwrap_or_default(),
            name: r.name,
            mime_type: r.mime_type,
            size: r.size,
            path: r.path,
            conversation_id: r.conversation_id.and_then(|s| s.parse().ok()),
            created_at: r.created_at,
        }
    }
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub properties: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphEdge {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub relation: String,
    pub weight: f64,
    pub properties: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphNeighbor {
    pub edge_id: String,
    pub relation: String,
    pub direction: String, // "incoming" or "outgoing"
    pub node: GraphNode,
}

#[derive(sqlx::FromRow)]
struct GraphNodeRow {
    id: String,
    label: String,
    #[sqlx(rename = "type")]
    node_type: String,
    properties: String,
    #[allow(dead_code)]
    created_at: String,
    #[allow(dead_code)]
    updated_at: String,
}

impl From<GraphNodeRow> for GraphNode {
    fn from(r: GraphNodeRow) -> Self {
        GraphNode {
            id: r.id,
            label: r.label,
            node_type: r.node_type,
            properties: serde_json::from_str(&r.properties).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse graph node properties: {e}");
                serde_json::Value::default()
            }),
        }
    }
}

#[derive(sqlx::FromRow)]
struct GraphEdgeRow {
    id: String,
    source_id: String,
    target_id: String,
    relation: String,
    weight: f64,
    properties: String,
    #[allow(dead_code)]
    created_at: String,
    #[allow(dead_code)]
    updated_at: String,
}

impl From<GraphEdgeRow> for GraphEdge {
    fn from(r: GraphEdgeRow) -> Self {
        GraphEdge {
            id: r.id,
            source_id: r.source_id,
            target_id: r.target_id,
            relation: r.relation,
            weight: r.weight,
            properties: serde_json::from_str(&r.properties).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse graph edge properties: {e}");
                serde_json::Value::default()
            }),
        }
    }
}

#[derive(sqlx::FromRow)]
struct GraphNeighborRow {
    edge_id: String,
    relation: String,
    #[allow(dead_code)]
    weight: f64,
    node_id: String,
    label: String,
    node_type: String,
    node_properties: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct IntegrationAccount {
    pub id: String,
    pub integration: String,
    pub name: String,
    pub data: String,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntegrationEvent {
    pub id: String,
    pub integration: String,
    pub account_id: String,
    pub event_type: String,
    pub dedupe_key: String,
    pub payload: serde_json::Value,
    pub status: String,
    pub created_at: String,
    pub processed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TelegramChatLink {
    pub chat_id: i64,
    pub conversation_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct ConversationRow {
    id: String,
    title: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct ConversationIdRow {
    id: String,
}

fn build_memory_search_queries(query: &str) -> Vec<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut queries = vec![trimmed.to_string()];

    let terms = extract_memory_search_terms(trimmed);
    if terms.is_empty() {
        return queries;
    }

    for prefix in ["key:", "tags:", ""] {
        let q = terms
            .iter()
            .map(|term| format!("{prefix}{term}*"))
            .collect::<Vec<_>>()
            .join(" OR ");
        queries.push(q);
    }

    queries
}

fn extract_memory_search_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = HashSet::new();

    for token in query.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')) {
        if token.len() < 2 {
            continue;
        }
        let lowered = token.to_ascii_lowercase();
        if seen.insert(lowered.clone()) {
            terms.push(lowered);
        }
    }

    terms
}

impl From<ConversationRow> for Conversation {
    fn from(r: ConversationRow) -> Self {
        Conversation {
            id: r.id.parse().unwrap_or_else(|e| {
                tracing::warn!("Failed to parse conversation id '{}': {e}", r.id);
                Uuid::default()
            }),
            title: r.title,
            created_at: r.created_at.parse().unwrap_or_else(|e| {
                tracing::warn!(
                    "Failed to parse conversation created_at '{}': {e}",
                    r.created_at
                );
                chrono::DateTime::default()
            }),
            updated_at: r.updated_at.parse().unwrap_or_else(|e| {
                tracing::warn!(
                    "Failed to parse conversation updated_at '{}': {e}",
                    r.updated_at
                );
                chrono::DateTime::default()
            }),
        }
    }
}

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: String,
    conversation_id: String,
    role: String,
    content: String,
    tool_calls: Option<String>,
    tool_call_id: Option<String>,
    name: Option<String>,
    created_at: String,
}

impl From<MessageRow> for Message {
    fn from(r: MessageRow) -> Self {
        Message {
            id: r.id.parse().unwrap_or_else(|e| {
                tracing::warn!("Failed to parse message id '{}': {e}", r.id);
                Uuid::default()
            }),
            conversation_id: r.conversation_id.parse().unwrap_or_else(|e| {
                tracing::warn!(
                    "Failed to parse message conversation_id '{}': {e}",
                    r.conversation_id
                );
                Uuid::default()
            }),
            role: r.role.parse().unwrap_or(Role::User),
            content: r.content,
            tool_calls: r.tool_calls.and_then(|s| {
                serde_json::from_str(&s)
                    .map_err(|e| {
                        tracing::warn!("Failed to parse tool_calls JSON: {e}");
                        e
                    })
                    .ok()
            }),
            tool_call_id: r.tool_call_id,
            name: r.name,
            attachments: None, // Populated separately in get_messages
            created_at: r.created_at.parse().unwrap_or_else(|e| {
                tracing::warn!("Failed to parse message created_at '{}': {e}", r.created_at);
                chrono::DateTime::default()
            }),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryEntry {
    pub key: String,
    pub content: String,
    pub tags: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryKeyInfo {
    pub key: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryEntryWithMetadata {
    pub key: String,
    pub content: String,
    pub tags: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MemoryKeyRow {
    key: String,
    created_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct MemoryEntryMetadataRow {
    key: String,
    content: String,
    tags: String,
    created_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct MemoryRow {
    key: String,
    content: String,
    tags: String,
}

#[derive(sqlx::FromRow)]
struct TelegramChatRow {
    conversation_id: String,
}

#[derive(sqlx::FromRow)]
struct TelegramChatLatestRow {
    telegram_chat_id: i64,
    conversation_id: String,
}

#[derive(sqlx::FromRow)]
struct TelegramChatIdRow {
    telegram_chat_id: i64,
}

#[derive(sqlx::FromRow)]
struct IntegrationEventRow {
    id: String,
    integration: String,
    account_id: String,
    event_type: String,
    dedupe_key: String,
    payload: String,
    status: String,
    created_at: String,
    processed_at: Option<String>,
    last_error: Option<String>,
}

impl From<IntegrationEventRow> for IntegrationEvent {
    fn from(r: IntegrationEventRow) -> Self {
        IntegrationEvent {
            id: r.id,
            integration: r.integration,
            account_id: r.account_id,
            event_type: r.event_type,
            dedupe_key: r.dedupe_key,
            payload: serde_json::from_str(&r.payload).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse integration event payload: {e}");
                serde_json::Value::default()
            }),
            status: r.status,
            created_at: r.created_at,
            processed_at: r.processed_at,
            last_error: r.last_error,
        }
    }
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    value: String,
}

#[derive(sqlx::FromRow)]
struct SettingsRowAll {
    key: String,
    value: String,
}

#[derive(sqlx::FromRow)]
struct SchemaEntryRow {
    entry_type: String,
    name: String,
    tbl_name: String,
    rootpage: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    async fn test_db() -> Database {
        let db = Database::new("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        db
    }

    fn temp_db_path(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("jossie-db-{test_name}-{}.sqlite", Uuid::new_v4()))
    }

    fn sqlite_url(path: &Path) -> String {
        format!("sqlite:{}?mode=rwc", path.display())
    }

    #[tokio::test]
    async fn create_and_get_conversation() {
        let db = test_db().await;
        let conv = db.create_conversation(Some("Test")).await.unwrap();
        assert_eq!(conv.title.as_deref(), Some("Test"));

        let fetched = db.get_conversation(conv.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, conv.id);
        assert_eq!(fetched.title.as_deref(), Some("Test"));
    }

    #[tokio::test]
    async fn list_conversations_ordering() {
        let db = test_db().await;
        let c1 = db.create_conversation(Some("First")).await.unwrap();
        let c2 = db.create_conversation(Some("Second")).await.unwrap();
        let list = db.list_conversations().await.unwrap();
        assert_eq!(list.len(), 2);
        // Most recent first
        assert_eq!(list[0].id, c2.id);
        assert_eq!(list[1].id, c1.id);
    }

    #[tokio::test]
    async fn save_and_get_messages() {
        let db = test_db().await;
        let conv = db.create_conversation(None).await.unwrap();

        let msg1 = Message {
            id: Uuid::new_v4(),
            conversation_id: conv.id,
            role: Role::User,
            content: "Hello".to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            created_at: Utc::now(),
        };
        db.save_message(&msg1).await.unwrap();

        let msg2 = Message {
            id: Uuid::new_v4(),
            conversation_id: conv.id,
            role: Role::Assistant,
            content: "Hi there".to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            created_at: Utc::now(),
        };
        db.save_message(&msg2).await.unwrap();

        let messages = db.get_messages(conv.id, None).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "Hello");
        assert_eq!(messages[1].content, "Hi there");
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn migrate_repairs_malformed_files_schema() {
        let db_path = temp_db_path("repair-files-rootpage");
        let db_url = sqlite_url(&db_path);

        let db = Database::new(&db_url).await.unwrap();
        db.migrate().await.unwrap();

        let mut conn = db.pool().acquire().await.unwrap();
        sqlx::query("PRAGMA writable_schema=ON")
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE sqlite_schema SET rootpage = 0 WHERE type = 'table' AND name = 'files'",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query("PRAGMA writable_schema=OFF")
            .execute(&mut *conn)
            .await
            .unwrap();
        drop(conn);
        drop(db);

        let repaired = Database::new(&db_url).await.unwrap();
        repaired.migrate().await.unwrap();

        let conv = repaired.create_conversation(Some("Files")).await.unwrap();
        let file_id = Uuid::new_v4();
        repaired
            .save_file_record(
                &file_id,
                "notes.txt",
                Some("text/plain"),
                12,
                "/tmp/notes.txt",
                Some(conv.id),
            )
            .await
            .unwrap();

        let files = repaired.list_files_for_conversation(conv.id).await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, file_id);

        let backup_parent = db_path.parent().unwrap();
        let original_name = db_path.file_name().unwrap().to_string_lossy();
        let backups = fs::read_dir(backup_parent)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{original_name}.repair-backup-"))
            })
            .count();
        assert_eq!(backups, 1);

        drop(repaired);
        for suffix in ["", "-wal", "-shm"] {
            fs::remove_file(sidecar_path(&db_path, suffix)).ok();
        }
        for entry in fs::read_dir(backup_parent)
            .unwrap()
            .filter_map(|entry| entry.ok())
        {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with(&format!("{original_name}.repair-backup-")) {
                fs::remove_file(entry.path()).ok();
            }
        }
    }

    #[tokio::test]
    async fn save_message_rejects_unknown_conversation() {
        let db = test_db().await;
        let msg = Message {
            id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            role: Role::User,
            content: "Hello".to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            created_at: Utc::now(),
        };

        let err = db.save_message(&msg).await.unwrap_err().to_string();
        assert!(
            err.contains("FOREIGN KEY constraint failed"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn get_nonexistent_conversation() {
        let db = test_db().await;
        let result = db.get_conversation(Uuid::new_v4()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn memory_save_and_search() {
        let db = test_db().await;
        db.memory_save("greeting", "Hello world, this is a test", "test greeting")
            .await
            .unwrap();
        db.memory_save("farewell", "Goodbye cruel world", "test farewell")
            .await
            .unwrap();

        let results = db.memory_search("hello").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "greeting");
    }

    #[tokio::test]
    async fn memory_save_overwrites() {
        let db = test_db().await;
        db.memory_save("key1", "original content", "")
            .await
            .unwrap();
        db.memory_save("key1", "updated content", "").await.unwrap();

        let results = db.memory_search("updated").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "updated content");
    }

    #[tokio::test]
    async fn memory_list() {
        let db = test_db().await;
        db.memory_save("k1", "c1", "t1").await.unwrap();
        db.memory_save("k2", "c2", "t2").await.unwrap();

        let keys = db.memory_list_keys().await.unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().any(|k| k.key == "k1"));
        assert!(keys.iter().any(|k| k.key == "k2"));

        let all = db.memory_list_all(10).await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|e| e.key == "k1" && e.content == "c1"));
        assert!(all.iter().any(|e| e.key == "k2" && e.content == "c2"));
    }

    #[tokio::test]
    async fn memory_search_falls_back_for_broad_keyword_queries() {
        let db = test_db().await;
        db.memory_save(
            "calendar.next_appointment",
            "Dentist appointment next Tuesday at 3pm",
            "calendar appointment health",
        )
        .await
        .unwrap();
        db.memory_save(
            "email.preferences",
            "User prefers concise email summaries with deadlines called out first",
            "email preferences work",
        )
        .await
        .unwrap();

        let results = db
            .memory_search(
                "actionable items deadlines upcoming appointments user calendar email preferences",
            )
            .await
            .unwrap();

        assert!(!results.is_empty());
        assert!(
            results
                .iter()
                .any(|entry| entry.key == "calendar.next_appointment")
        );
        assert!(results.iter().any(|entry| entry.key == "email.preferences"));
    }

    #[test]
    fn memory_search_query_builder_adds_fallback_strategies() {
        let queries = build_memory_search_queries(
            "actionable items deadlines upcoming appointments user calendar email preferences",
        );

        assert_eq!(
            queries[0],
            "actionable items deadlines upcoming appointments user calendar email preferences"
        );
        assert!(queries.iter().any(|query| query.contains("key:calendar*")));
        assert!(queries.iter().any(|query| query.contains("tags:email*")));
        assert!(
            queries
                .iter()
                .any(|query| query.contains("calendar* OR email*"))
        );
    }

    #[tokio::test]
    async fn link_and_get_telegram_conversation() {
        let db = test_db().await;
        let conv = db.create_conversation(Some("TG Chat")).await.unwrap();
        let chat_id = 123456789;

        db.link_telegram_conversation(chat_id, conv.id)
            .await
            .unwrap();
        let linked_id = db
            .get_telegram_conversation(chat_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(linked_id, conv.id);

        let unknown = db.get_telegram_conversation(987654321).await.unwrap();
        assert!(unknown.is_none());

        let by_conv = db
            .get_telegram_chat_for_conversation(conv.id)
            .await
            .unwrap();
        assert_eq!(by_conv, Some(chat_id));

        let unknown_conv = db
            .get_telegram_chat_for_conversation(Uuid::new_v4())
            .await
            .unwrap();
        assert!(unknown_conv.is_none());
    }

    #[tokio::test]
    async fn scheduled_task_claim_and_reschedule() {
        let db = test_db().await;
        let conv = db.create_conversation(Some("Tasks")).await.unwrap();
        let now = Utc::now();
        let task_data = serde_json::json!({"prompt": "Do thing"});

        let task_id = db
            .create_scheduled_task(
                conv.id,
                "agent_run",
                &task_data,
                "interval",
                "60",
                Some(&now.to_rfc3339()),
                None,
            )
            .await
            .unwrap();

        assert!(db.mark_task_running_if_pending(&task_id).await.unwrap());
        assert!(!db.mark_task_running_if_pending(&task_id).await.unwrap());

        let retry = (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339();
        db.update_task_next_run(&task_id, &retry, false)
            .await
            .unwrap();
        let task = db.get_scheduled_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, "pending");
        assert_eq!(task.run_count, 0);

        assert!(db.mark_task_running_if_pending(&task_id).await.unwrap());
        let next_run = (Utc::now() + chrono::Duration::seconds(90)).to_rfc3339();
        db.update_task_next_run(&task_id, &next_run, true)
            .await
            .unwrap();
        let task = db.get_scheduled_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, "pending");
        assert_eq!(task.run_count, 1);
    }

    #[tokio::test]
    async fn oob_priority_ordering_uses_priority_rank() {
        let db = test_db().await;
        let conv = db.create_conversation(Some("OOB")).await.unwrap();

        db.queue_oob_message(conv.id, "normal msg", "normal")
            .await
            .unwrap();
        db.queue_oob_message(conv.id, "urgent msg", "urgent")
            .await
            .unwrap();
        db.queue_oob_message(conv.id, "high msg", "high")
            .await
            .unwrap();

        let queued = db.list_pending_oob_messages(10).await.unwrap();
        let contents: Vec<&str> = queued.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["urgent msg", "high msg", "normal msg"]);
    }

    #[tokio::test]
    async fn test_integration_settings() {
        let db = test_db().await;
        let integration = "google";
        db.set_integration_setting(integration, "refresh_token", "abc")
            .await
            .unwrap();
        db.set_integration_setting(integration, "other", "123")
            .await
            .unwrap();

        let val = db
            .get_integration_setting(integration, "refresh_token")
            .await
            .unwrap();
        assert_eq!(val.as_deref(), Some("abc"));

        let all = db.get_all_integration_settings(integration).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.get("refresh_token").map(|s| s.as_str()), Some("abc"));
    }

    #[tokio::test]
    async fn test_integration_accounts() {
        let db = test_db().await;
        let data = serde_json::json!({"foo": "bar"});
        let id = db
            .add_integration_account("test_int", "My Account", &data)
            .await
            .unwrap();

        let acc = db.get_integration_account(&id).await.unwrap().unwrap();
        assert_eq!(acc.name, "My Account");
        assert_eq!(acc.data, data.to_string());

        let list = db.list_integration_accounts("test_int").await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);

        db.delete_integration_account(&id).await.unwrap();
        let list = db.list_integration_accounts("test_int").await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn integration_event_dedupe_and_processing() {
        let db = test_db().await;
        let payload = serde_json::json!({"foo": "bar"});

        let inserted = db
            .insert_integration_event("google", "acc1", "gmail_new_message", "msg1", &payload)
            .await
            .unwrap();
        assert!(inserted);

        let dup = db
            .insert_integration_event("google", "acc1", "gmail_new_message", "msg1", &payload)
            .await
            .unwrap();
        assert!(!dup);

        let pending = db.list_pending_integration_events(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].dedupe_key, "msg1");

        db.mark_integration_event_processed(&pending[0].id)
            .await
            .unwrap();
        let pending_after = db.list_pending_integration_events(10).await.unwrap();
        assert!(pending_after.is_empty());
    }

    #[tokio::test]
    async fn graph_upsert_and_search_nodes() {
        let db = test_db().await;
        db.graph_upsert_node(
            "robin",
            "Robin Decker",
            "Person",
            &serde_json::json!({"email": "robin@example.com"}),
        )
        .await
        .unwrap();

        let found = db.graph_find_nodes("Robin").await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "robin");
        assert_eq!(found[0].label, "Robin Decker");
        assert_eq!(found[0].node_type, "Person");
    }

    #[tokio::test]
    async fn graph_edges_and_neighbors() {
        let db = test_db().await;
        db.graph_upsert_node("robin", "Robin", "Person", &serde_json::json!({}))
            .await
            .unwrap();
        db.graph_upsert_node("apollo", "Apollo", "Project", &serde_json::json!({}))
            .await
            .unwrap();

        db.graph_upsert_edge("robin", "apollo", "WORKS_ON", 0.9, &serde_json::json!({}))
            .await
            .unwrap();

        let robin_neighbors = db.graph_get_neighbors("robin").await.unwrap();
        assert!(
            robin_neighbors
                .iter()
                .any(|n| n.node.id == "apollo" && n.direction == "outgoing")
        );

        let apollo_neighbors = db.graph_get_neighbors("apollo").await.unwrap();
        assert!(
            apollo_neighbors
                .iter()
                .any(|n| n.node.id == "robin" && n.direction == "incoming")
        );
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub conversation_id: String,
    pub task_type: String,
    pub task_data: serde_json::Value,
    pub schedule_type: String,
    pub schedule_value: String,
    pub status: String,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub run_count: i64,
    pub max_runs: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub last_error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ScheduledTaskRow {
    id: String,
    conversation_id: String,
    task_type: String,
    task_data: String,
    schedule_type: String,
    schedule_value: String,
    status: String,
    next_run_at: Option<String>,
    last_run_at: Option<String>,
    run_count: i64,
    max_runs: Option<i64>,
    created_at: String,
    updated_at: String,
    last_error: Option<String>,
}

impl From<ScheduledTaskRow> for ScheduledTask {
    fn from(r: ScheduledTaskRow) -> Self {
        ScheduledTask {
            id: r.id,
            conversation_id: r.conversation_id,
            task_type: r.task_type,
            task_data: serde_json::from_str(&r.task_data).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse scheduled task data: {e}");
                serde_json::Value::default()
            }),
            schedule_type: r.schedule_type,
            schedule_value: r.schedule_value,
            status: r.status,
            next_run_at: r.next_run_at,
            last_run_at: r.last_run_at,
            run_count: r.run_count,
            max_runs: r.max_runs,
            created_at: r.created_at,
            updated_at: r.updated_at,
            last_error: r.last_error,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutOfBandMessage {
    pub id: String,
    pub conversation_id: String,
    pub sender: String,
    pub content: String,
    pub priority: String,
    pub status: String,
    pub created_at: String,
    pub sent_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct OutOfBandMessageRow {
    id: String,
    conversation_id: String,
    sender: String,
    content: String,
    priority: String,
    status: String,
    created_at: String,
    sent_at: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConversationSummary {
    pub conversation_id: String,
    pub summary: String,
    pub messages_summarized: i64,
    pub last_message_id: Option<String>,
    pub created_at: String,
}

#[derive(sqlx::FromRow)]
struct ConversationSummaryRow {
    conversation_id: String,
    summary: String,
    messages_summarized: i64,
    last_message_id: Option<String>,
    created_at: String,
}

impl From<ConversationSummaryRow> for ConversationSummary {
    fn from(r: ConversationSummaryRow) -> Self {
        ConversationSummary {
            conversation_id: r.conversation_id,
            summary: r.summary,
            messages_summarized: r.messages_summarized,
            last_message_id: r.last_message_id,
            created_at: r.created_at,
        }
    }
}

impl From<OutOfBandMessageRow> for OutOfBandMessage {
    fn from(r: OutOfBandMessageRow) -> Self {
        OutOfBandMessage {
            id: r.id,
            conversation_id: r.conversation_id,
            sender: r.sender,
            content: r.content,
            priority: r.priority,
            status: r.status,
            created_at: r.created_at,
            sent_at: r.sent_at,
            last_error: r.last_error,
        }
    }
}
