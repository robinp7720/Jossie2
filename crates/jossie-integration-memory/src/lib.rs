use jossie_core::integration::{Integration, ToolDefinition};
use jossie_db::Database;
use serde::Deserialize;
use std::sync::Arc;

pub struct MemoryIntegration {
    db: Arc<Database>,
}

impl MemoryIntegration {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl Integration for MemoryIntegration {
    fn name(&self) -> &str {
        "memory"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "memory_save".to_string(),
                description: "Save information to long-term memory with a key and optional tags"
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "key": {"type": "string", "description": "Unique key for this memory"},
                        "content": {"type": "string", "description": "Content to remember"},
                        "tags": {"type": "string", "description": "Space-separated tags for categorization (use empty string for none)"}
                    },
                    "required": ["key", "content", "tags"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "memory_search".to_string(),
                description: "Search long-term memory using full-text search".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query"}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "memory_list_keys".to_string(),
                description: "List all keys stored in long-term memory with timestamps".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "memory_list_all".to_string(),
                description: "List all memories with full content".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "limit": {"type": "integer", "description": "Number of memories to return (default 50, max 500)"}
                    },
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        match tool_name {
            "memory_save" => {
                #[derive(Deserialize)]
                struct Args {
                    key: String,
                    content: String,
                    #[serde(default)]
                    tags: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.db
                    .memory_save(&args.key, &args.content, &args.tags)
                    .await?;
                Ok(format!("Saved memory with key '{}'", args.key))
            }
            "memory_search" => {
                #[derive(Deserialize)]
                struct Args {
                    query: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let results = self.db.memory_search(&args.query).await?;
                Ok(serde_json::to_string_pretty(&results)?)
            }
            "memory_list_keys" => {
                let results = self.db.memory_list_keys().await?;
                Ok(serde_json::to_string_pretty(&results)?)
            }
            "memory_list_all" => {
                #[derive(Deserialize)]
                struct Args {
                    #[serde(default = "default_limit")]
                    limit: usize,
                }
                fn default_limit() -> usize {
                    50
                }
                let args: Args = serde_json::from_str(arguments).unwrap_or(Args { limit: 50 });
                let results = self.db.memory_list_all(args.limit).await?;
                Ok(serde_json::to_string_pretty(&results)?)
            }
            _ => anyhow::bail!("Unknown tool: {tool_name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jossie_core::integration::Integration;
    use jossie_db::Database;

    async fn test_memory() -> MemoryIntegration {
        let db = Database::new("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        MemoryIntegration::new(Arc::new(db))
    }

    #[tokio::test]
    async fn tools_are_defined() {
        let mem = test_memory().await;
        let tools = mem.tools();
        assert_eq!(tools.len(), 4);
        assert!(tools.iter().any(|t| t.name == "memory_save"));
        assert!(tools.iter().any(|t| t.name == "memory_search"));
        assert!(tools.iter().any(|t| t.name == "memory_list_keys"));
        assert!(tools.iter().any(|t| t.name == "memory_list_all"));
    }

    #[tokio::test]
    async fn save_and_search() {
        let mem = test_memory().await;

        let save_result = mem
            .execute(
                "memory_save",
                r#"{"key":"test","content":"important info","tags":"test"}"#,
            )
            .await
            .unwrap();
        assert!(save_result.contains("Saved"));

        let search_result = mem
            .execute("memory_search", r#"{"query":"important"}"#)
            .await
            .unwrap();
        assert!(search_result.contains("important info"));
    }

    #[tokio::test]
    async fn list_keys_and_all() {
        let mem = test_memory().await;

        mem.execute("memory_save", r#"{"key":"k1","content":"c1","tags":"t1"}"#)
            .await
            .unwrap();
        mem.execute("memory_save", r#"{"key":"k2","content":"c2","tags":"t2"}"#)
            .await
            .unwrap();

        let keys_json = mem.execute("memory_list_keys", "{}").await.unwrap();
        let keys: Vec<serde_json::Value> = serde_json::from_str(&keys_json).unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().any(|k| k["key"] == "k1"));
        assert!(keys.iter().any(|k| k["key"] == "k2"));

        let all_json = mem.execute("memory_list_all", "{}").await.unwrap();
        let all: Vec<serde_json::Value> = serde_json::from_str(&all_json).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|e| e["key"] == "k1" && e["content"] == "c1"));
        assert!(all.iter().any(|e| e["key"] == "k2" && e["content"] == "c2"));
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let mem = test_memory().await;
        let result = mem.execute("nonexistent", "{}").await;
        assert!(result.is_err());
    }
}
