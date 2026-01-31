use jossie_core::types::{Conversation, Message, Role};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use uuid::Uuid;
use chrono::Utc;
use std::collections::HashMap;

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn new(url: &str) -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query(include_str!("../../jossie-db/migrations.sql"))
            .execute(&self.pool)
            .await
            .ok(); // ignore if already exists
        Ok(())
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // Conversations
    pub async fn create_conversation(&self, title: Option<&str>) -> anyhow::Result<Conversation> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let id_str = id.to_string();
        let now_str = now.to_rfc3339();
        sqlx::query("INSERT INTO conversations (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind(&id_str)
            .bind(title)
            .bind(&now_str)
            .bind(&now_str)
            .execute(&self.pool)
            .await?;
        Ok(Conversation { id, title: title.map(String::from), created_at: now, updated_at: now })
    }

    pub async fn get_conversation(&self, id: Uuid) -> anyhow::Result<Option<Conversation>> {
        let id_str = id.to_string();
        let row = sqlx::query_as::<_, ConversationRow>("SELECT id, title, created_at, updated_at FROM conversations WHERE id = ?")
            .bind(&id_str)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.into()))
    }

    pub async fn list_conversations(&self) -> anyhow::Result<Vec<Conversation>> {
        let rows = sqlx::query_as::<_, ConversationRow>("SELECT id, title, created_at, updated_at FROM conversations ORDER BY updated_at DESC")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    // Messages
    pub async fn save_message(&self, msg: &Message) -> anyhow::Result<()> {
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
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_messages(&self, conversation_id: Uuid) -> anyhow::Result<Vec<Message>> {
        let conv_str = conversation_id.to_string();
        let rows = sqlx::query_as::<_, MessageRow>("SELECT id, conversation_id, role, content, tool_calls, tool_call_id, name, created_at FROM messages WHERE conversation_id = ? ORDER BY created_at ASC")
            .bind(&conv_str)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    // Memory (FTS5)
    pub async fn memory_save(&self, key: &str, content: &str, tags: &str) -> anyhow::Result<()> {
        // Delete existing entry if any
        sqlx::query("DELETE FROM memory WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await.ok();
        sqlx::query("INSERT INTO memory (key, content, tags) VALUES (?, ?, ?)")
            .bind(key)
            .bind(content)
            .bind(tags)
            .execute(&self.pool)
            .await?;
        let now_str = Utc::now().to_rfc3339();
        sqlx::query("INSERT OR REPLACE INTO memory_metadata (key, created_at, updated_at) VALUES (?, ?, ?)")
            .bind(key)
            .bind(&now_str)
            .bind(&now_str)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn memory_search(&self, query: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        let rows = sqlx::query_as::<_, MemoryRow>("SELECT key, content, tags FROM memory WHERE memory MATCH ? ORDER BY rank LIMIT 10")
            .bind(query)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| MemoryEntry { key: r.key, content: r.content, tags: r.tags }).collect())
    }

    pub async fn get_memory(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let row = sqlx::query_as::<_, MemoryRow>("SELECT key, content, tags FROM memory WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| MemoryEntry { key: r.key, content: r.content, tags: r.tags }))
    }

    // Telegram
    pub async fn get_telegram_conversation(&self, chat_id: i64) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query_as::<_, TelegramChatRow>("SELECT conversation_id FROM telegram_chats WHERE telegram_chat_id = ?")
            .bind(chat_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| r.conversation_id.parse().ok()))
    }

    pub async fn link_telegram_conversation(&self, chat_id: i64, conversation_id: Uuid) -> anyhow::Result<()> {
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

    // Integration Settings
    pub async fn get_integration_setting(&self, integration: &str, key: &str) -> anyhow::Result<Option<String>> {
        let row = sqlx::query_as::<_, SettingsRow>("SELECT value FROM integration_settings WHERE integration = ? AND key = ?")
            .bind(integration)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.value))
    }

    pub async fn set_integration_setting(&self, integration: &str, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query("INSERT OR REPLACE INTO integration_settings (integration, key, value) VALUES (?, ?, ?)")
            .bind(integration)
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_all_integration_settings(&self, integration: &str) -> anyhow::Result<HashMap<String, String>> {
        let rows = sqlx::query_as::<_, SettingsRowAll>("SELECT key, value FROM integration_settings WHERE integration = ?")
            .bind(integration)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| (r.key, r.value)).collect())
    }

    // Integration Accounts
    pub async fn add_integration_account(&self, integration: &str, name: &str, data: &serde_json::Value) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let data_str = serde_json::to_string(data)?;
        sqlx::query("INSERT INTO integration_accounts (id, integration, name, data) VALUES (?, ?, ?, ?)")
            .bind(&id)
            .bind(integration)
            .bind(name)
            .bind(&data_str)
            .execute(&self.pool)
            .await?;
        Ok(id)
    }

    pub async fn upsert_integration_account(&self, id: &str, integration: &str, name: &str, data: &serde_json::Value) -> anyhow::Result<()> {
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

    pub async fn get_integration_account(&self, id: &str) -> anyhow::Result<Option<IntegrationAccount>> {
        let row = sqlx::query_as::<_, IntegrationAccount>("SELECT id, integration, name, data, created_at FROM integration_accounts WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    pub async fn list_integration_accounts(&self, integration: &str) -> anyhow::Result<Vec<IntegrationAccount>> {
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
        let payload_str = serde_json::to_string(payload)?;
        let res = sqlx::query(
            "INSERT OR IGNORE INTO integration_events (id, integration, account_id, event_type, dedupe_key, payload) VALUES (?, ?, ?, ?, ?, ?)"
        )
        .bind(&id)
        .bind(integration)
        .bind(account_id)
        .bind(event_type)
        .bind(dedupe_key)
        .bind(&payload_str)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn list_pending_integration_events(&self, limit: usize) -> anyhow::Result<Vec<IntegrationEvent>> {
        let rows = sqlx::query_as::<_, IntegrationEventRow>(
            "SELECT id, integration, account_id, event_type, dedupe_key, payload, status, created_at, processed_at, last_error
             FROM integration_events
             WHERE status = 'new'
             ORDER BY created_at ASC
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

    pub async fn mark_integration_event_failed(&self, id: &str, error: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE integration_events SET status = 'failed', last_error = ? WHERE id = ?")
            .bind(error)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // Knowledge Graph

    pub async fn graph_upsert_node(&self, id: &str, label: &str, node_type: &str, properties: &serde_json::Value) -> anyhow::Result<()> {
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
                updated_at = excluded.updated_at"
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

    pub async fn graph_upsert_edge(&self, source_id: &str, target_id: &str, relation: &str, weight: f64, properties: &serde_json::Value) -> anyhow::Result<String> {
        // Check if edge exists with same source, target, relation
        // We'll treat (source, target, relation) as unique for simplicity in this iteration, 
        // though the DB schema uses a UUID PK.
        
        let props_str = serde_json::to_string(properties)?;
        let now_str = Utc::now().to_rfc3339();

        let existing = sqlx::query_as::<_, GraphEdgeRow>(
            "SELECT * FROM graph_edges WHERE source_id = ? AND target_id = ? AND relation = ?"
        )
        .bind(source_id)
        .bind(target_id)
        .bind(relation)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(edge) = existing {
            sqlx::query(
                "UPDATE graph_edges SET weight = ?, properties = ?, updated_at = ? WHERE id = ?"
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
        let rows = sqlx::query_as::<_, GraphNodeRow>("SELECT * FROM graph_nodes WHERE label LIKE ? LIMIT 20")
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
            "#
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
            "#
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
                    properties: serde_json::from_str(&r.node_properties).unwrap_or_default(),
                }
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
                    properties: serde_json::from_str(&r.node_properties).unwrap_or_default(),
                }
            });
        }

        Ok(results)
    }

    pub async fn graph_list_nodes(&self, limit: usize) -> anyhow::Result<Vec<GraphNode>> {
        let limit = limit.max(1).min(5000);
        let rows = sqlx::query_as::<_, GraphNodeRow>(
            "SELECT * FROM graph_nodes ORDER BY updated_at DESC LIMIT ?"
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn graph_list_edges(&self, limit: usize) -> anyhow::Result<Vec<GraphEdge>> {
        let limit = limit.max(1).min(5000);
        let rows = sqlx::query_as::<_, GraphEdgeRow>(
            "SELECT * FROM graph_edges ORDER BY updated_at DESC LIMIT ?"
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

// Row types for sqlx
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
            properties: serde_json::from_str(&r.properties).unwrap_or_default(),
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
            properties: serde_json::from_str(&r.properties).unwrap_or_default(),
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

impl From<ConversationRow> for Conversation {
    fn from(r: ConversationRow) -> Self {
        Conversation {
            id: r.id.parse().unwrap_or_default(),
            title: r.title,
            created_at: r.created_at.parse().unwrap_or_default(),
            updated_at: r.updated_at.parse().unwrap_or_default(),
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
            id: r.id.parse().unwrap_or_default(),
            conversation_id: r.conversation_id.parse().unwrap_or_default(),
            role: r.role.parse().unwrap_or(Role::User),
            content: r.content,
            tool_calls: r.tool_calls.and_then(|s| serde_json::from_str(&s).ok()),
            tool_call_id: r.tool_call_id,
            name: r.name,
            created_at: r.created_at.parse().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryEntry {
    pub key: String,
    pub content: String,
    pub tags: String,
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
            payload: serde_json::from_str(&r.payload).unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> Database {
        let db = Database::new("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        db
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
            tool_calls: None,
            tool_call_id: None,
            name: None,
            created_at: Utc::now(),
        };
        db.save_message(&msg2).await.unwrap();

        let messages = db.get_messages(conv.id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "Hello");
        assert_eq!(messages[1].content, "Hi there");
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
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
        db.memory_save("greeting", "Hello world, this is a test", "test greeting").await.unwrap();
        db.memory_save("farewell", "Goodbye cruel world", "test farewell").await.unwrap();

        let results = db.memory_search("hello").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "greeting");
    }

    #[tokio::test]
    async fn memory_save_overwrites() {
        let db = test_db().await;
        db.memory_save("key1", "original content", "").await.unwrap();
        db.memory_save("key1", "updated content", "").await.unwrap();

        let results = db.memory_search("updated").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "updated content");
    }

    #[tokio::test]
    async fn link_and_get_telegram_conversation() {
        let db = test_db().await;
        let conv = db.create_conversation(Some("TG Chat")).await.unwrap();
        let chat_id = 123456789;

        db.link_telegram_conversation(chat_id, conv.id).await.unwrap();
        let linked_id = db.get_telegram_conversation(chat_id).await.unwrap().unwrap();
        assert_eq!(linked_id, conv.id);

        let unknown = db.get_telegram_conversation(987654321).await.unwrap();
        assert!(unknown.is_none());
    }

    #[tokio::test]
    async fn test_integration_settings() {
        let db = test_db().await;
        let integration = "google";
        db.set_integration_setting(integration, "refresh_token", "abc").await.unwrap();
        db.set_integration_setting(integration, "other", "123").await.unwrap();

        let val = db.get_integration_setting(integration, "refresh_token").await.unwrap();
        assert_eq!(val.as_deref(), Some("abc"));

        let all = db.get_all_integration_settings(integration).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.get("refresh_token").map(|s| s.as_str()), Some("abc"));
    }

    #[tokio::test]
    async fn test_integration_accounts() {
        let db = test_db().await;
        let data = serde_json::json!({"foo": "bar"});
        let id = db.add_integration_account("test_int", "My Account", &data).await.unwrap();

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

        db.mark_integration_event_processed(&pending[0].id).await.unwrap();
        let pending_after = db.list_pending_integration_events(10).await.unwrap();
        assert!(pending_after.is_empty());
    }

    #[tokio::test]
    async fn graph_upsert_and_search_nodes() {
        let db = test_db().await;
        db.graph_upsert_node("robin", "Robin Decker", "Person", &serde_json::json!({"email": "robin@example.com"}))
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
        db.graph_upsert_node("robin", "Robin", "Person", &serde_json::json!({})).await.unwrap();
        db.graph_upsert_node("apollo", "Apollo", "Project", &serde_json::json!({})).await.unwrap();

        db.graph_upsert_edge("robin", "apollo", "WORKS_ON", 0.9, &serde_json::json!({}))
            .await
            .unwrap();

        let robin_neighbors = db.graph_get_neighbors("robin").await.unwrap();
        assert!(robin_neighbors.iter().any(|n| n.node.id == "apollo" && n.direction == "outgoing"));

        let apollo_neighbors = db.graph_get_neighbors("apollo").await.unwrap();
        assert!(apollo_neighbors.iter().any(|n| n.node.id == "robin" && n.direction == "incoming"));
    }
}
