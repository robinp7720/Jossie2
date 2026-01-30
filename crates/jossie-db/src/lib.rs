use jossie_core::types::{Conversation, Message, Role};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use uuid::Uuid;
use chrono::Utc;

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
}

// Row types for sqlx
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
}
