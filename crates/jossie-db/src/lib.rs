use anyhow::Context;
use chrono::{Duration, Utc};
use jossie_core::types::{Conversation, Message, Role};
use sqlx::QueryBuilder;
use sqlx::sqlite::{Sqlite, SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

mod work;
pub use work::*;

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
        // Split migrations into individual statements and run each one. The
        // splitter must understand comments and quoted values because those may
        // legitimately contain semicolons.
        // IF NOT EXISTS clauses handle idempotency; real errors are propagated.
        let sql = include_str!("../../jossie-db/migrations.sql");
        for statement in split_sql_statements(sql) {
            let statement = statement.trim();
            if statement.is_empty() {
                continue;
            }
            if let Err(e) = sqlx::query(statement).execute(&self.pool).await {
                // FTS5 virtual tables can't use IF NOT EXISTS, so ignore "already exists" errors
                let msg = e.to_string();
                if msg.contains("already exists") || msg.contains("duplicate column name") {
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
}

#[path = "repositories/auth_activity.rs"]
mod auth_activity;
#[path = "repositories/conversations.rs"]
mod conversations;
#[path = "repositories/files_actions.rs"]
mod files_actions;
#[path = "repositories/graph.rs"]
mod graph;
#[path = "repositories/integrations.rs"]
mod integrations;
#[path = "repositories/memory.rs"]
mod memory;
#[path = "repositories/memory_prompt.rs"]
mod memory_prompt;
#[path = "repositories/scheduler.rs"]
mod scheduler;

fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut statement = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;

    while let Some(ch) = chars.next() {
        statement.push(ch);

        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
            }
            continue;
        }

        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                statement.push(chars.next().expect("peeked block-comment terminator"));
                in_block_comment = false;
            }
            continue;
        }

        if in_single_quote {
            if ch == '\'' {
                if chars.peek() == Some(&'\'') {
                    statement.push(chars.next().expect("peeked escaped quote"));
                } else {
                    in_single_quote = false;
                }
            }
            continue;
        }

        if in_double_quote {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    statement.push(chars.next().expect("peeked escaped identifier quote"));
                } else {
                    in_double_quote = false;
                }
            }
            continue;
        }

        match ch {
            '-' if chars.peek() == Some(&'-') => {
                statement.push(chars.next().expect("peeked line-comment marker"));
                in_line_comment = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                statement.push(chars.next().expect("peeked block-comment marker"));
                in_block_comment = true;
            }
            '\'' => in_single_quote = true,
            '"' => in_double_quote = true,
            ';' => {
                statements.push(std::mem::take(&mut statement));
            }
            _ => {}
        }
    }

    if !statement.trim().is_empty() {
        statements.push(statement);
    }

    statements
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

fn normalize_prompt_scope(scope: &str) -> String {
    match scope.trim().to_ascii_lowercase().as_str() {
        "chat" => "chat".to_string(),
        "event" | "events" => "event".to_string(),
        "both" | "all" => "both".to_string(),
        _ => "none".to_string(),
    }
}

fn normalize_memory_importance(importance: i64) -> i64 {
    importance.clamp(0, 100)
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct ChatImport {
    pub id: String,
    pub file_id: String,
    pub format: String,
    pub status: String,
    pub total_messages: i64,
    pub analyzed_messages: i64,
    pub memories_saved: i64,
    pub nodes_saved: i64,
    pub edges_saved: i64,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
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

#[derive(sqlx::FromRow)]
struct MessageAttachmentFileRow {
    message_id: String,
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

impl From<MessageAttachmentFileRow> for FileRecord {
    fn from(r: MessageAttachmentFileRow) -> Self {
        FileRecord {
            id: r.id.parse().unwrap_or_default(),
            name: r.name,
            mime_type: r.mime_type,
            size: r.size,
            path: r.path,
            conversation_id: r.conversation_id.and_then(|id| id.parse().ok()),
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

#[derive(sqlx::FromRow)]
struct GraphContextNeighborRow {
    root_id: String,
    direction: String,
    edge_id: String,
    relation: String,
    #[allow(dead_code)]
    weight: f64,
    node_id: String,
    label: String,
    node_type: String,
    node_properties: String,
}

impl From<GraphContextNeighborRow> for GraphNeighbor {
    fn from(r: GraphContextNeighborRow) -> Self {
        GraphNeighbor {
            edge_id: r.edge_id,
            relation: r.relation,
            direction: r.direction,
            node: GraphNode {
                id: r.node_id,
                label: r.label,
                node_type: r.node_type,
                properties: serde_json::from_str(&r.node_properties).unwrap_or_default(),
            },
        }
    }
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

    let terms = extract_memory_search_terms(trimmed);
    if terms.is_empty() {
        return Vec::new();
    }

    let mut queries = Vec::new();
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

    for token in query.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')) {
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
            response_items: None,
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
    pub prompt_scope: String,
    pub importance: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryStats {
    pub total: i64,
    pub prompt_ready: i64,
}

#[derive(Debug, Clone)]
pub struct AuthSession {
    pub id: String,
    pub token_hash: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ActivityEvent {
    pub id: String,
    pub conversation_id: Option<Uuid>,
    pub run_id: Option<String>,
    pub category: String,
    pub title: String,
    pub detail: Option<String>,
    pub tone: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct NewPendingAction {
    pub batch_id: String,
    pub conversation_id: Uuid,
    pub run_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub arguments: String,
    pub title: String,
    pub summary: String,
    pub effect: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingAction {
    pub id: String,
    pub batch_id: String,
    pub conversation_id: Uuid,
    pub run_id: String,
    pub call_id: String,
    pub tool_name: String,
    #[serde(skip_serializing)]
    pub arguments: String,
    pub title: String,
    pub summary: String,
    pub effect: String,
    pub status: String,
    pub result_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub resolved_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PendingActionRow {
    id: String,
    batch_id: String,
    conversation_id: String,
    run_id: String,
    call_id: String,
    tool_name: String,
    arguments: String,
    title: String,
    summary: String,
    effect: String,
    status: String,
    result_error: Option<String>,
    created_at: String,
    updated_at: String,
    resolved_at: Option<String>,
}

impl From<PendingActionRow> for PendingAction {
    fn from(row: PendingActionRow) -> Self {
        Self {
            id: row.id,
            batch_id: row.batch_id,
            conversation_id: row.conversation_id.parse().unwrap_or_default(),
            run_id: row.run_id,
            call_id: row.call_id,
            tool_name: row.tool_name,
            arguments: row.arguments,
            title: row.title,
            summary: row.summary,
            effect: row.effect,
            status: row.status,
            result_error: row.result_error,
            created_at: row.created_at,
            updated_at: row.updated_at,
            resolved_at: row.resolved_at,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryPromptEntry {
    pub key: String,
    pub content: String,
    pub tags: String,
    pub prompt_scope: String,
    pub importance: i64,
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
    prompt_scope: String,
    importance: i64,
}

#[derive(sqlx::FromRow)]
struct ActivityEventRow {
    id: String,
    conversation_id: Option<String>,
    run_id: Option<String>,
    category: String,
    title: String,
    detail: Option<String>,
    tone: String,
    created_at: String,
}

impl From<ActivityEventRow> for ActivityEvent {
    fn from(row: ActivityEventRow) -> Self {
        Self {
            id: row.id,
            conversation_id: row.conversation_id.and_then(|id| id.parse().ok()),
            run_id: row.run_id,
            category: row.category,
            title: row.title,
            detail: row.detail,
            tone: row.tone,
            created_at: row.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct MemoryPromptRow {
    key: String,
    content: String,
    tags: String,
    prompt_scope: String,
    importance: i64,
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
mod tests;

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
