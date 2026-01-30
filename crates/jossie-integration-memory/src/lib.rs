use std::sync::Arc;
use jossie_core::integration::{Integration, ToolDefinition};
use jossie_db::Database;
use serde::Deserialize;

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
                description: "Save information to long-term memory with a key and optional tags".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "key": {"type": "string", "description": "Unique key for this memory"},
                        "content": {"type": "string", "description": "Content to remember"},
                        "tags": {"type": "string", "description": "Space-separated tags for categorization"}
                    },
                    "required": ["key", "content"]
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
                    "required": ["query"]
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        match tool_name {
            "memory_save" => {
                #[derive(Deserialize)]
                struct Args { key: String, content: String, #[serde(default)] tags: String }
                let args: Args = serde_json::from_str(arguments)?;
                self.db.memory_save(&args.key, &args.content, &args.tags).await?;
                Ok(format!("Saved memory with key '{}'", args.key))
            }
            "memory_search" => {
                #[derive(Deserialize)]
                struct Args { query: String }
                let args: Args = serde_json::from_str(arguments)?;
                let results = self.db.memory_search(&args.query).await?;
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
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "memory_save");
        assert_eq!(tools[1].name, "memory_search");
    }

    #[tokio::test]
    async fn save_and_search() {
        let mem = test_memory().await;

        let save_result = mem.execute("memory_save", r#"{"key":"test","content":"important info","tags":"test"}"#).await.unwrap();
        assert!(save_result.contains("Saved"));

        let search_result = mem.execute("memory_search", r#"{"query":"important"}"#).await.unwrap();
        assert!(search_result.contains("important info"));
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let mem = test_memory().await;
        let result = mem.execute("nonexistent", "{}").await;
        assert!(result.is_err());
    }
}
