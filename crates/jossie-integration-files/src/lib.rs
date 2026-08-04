use jossie_core::integration::{Integration, ToolDefinition};
use jossie_db::Database;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

mod importer;

pub use importer::{ChatExportFormat, ChatExportImporter};

pub struct FilesIntegration {
    db: Arc<Database>,
    chat_importer: Arc<ChatExportImporter>,
}

impl FilesIntegration {
    pub fn new(db: Arc<Database>, chat_importer: Arc<ChatExportImporter>) -> Self {
        Self { db, chat_importer }
    }
}

#[async_trait::async_trait]
impl Integration for FilesIntegration {
    fn name(&self) -> &str {
        "files"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "list_files".to_string(),
                description: "List files attached to the current conversation.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "__conversation_id": {"type": "string", "description": "UUID of the conversation (autofilled)"}
                    },
                    "required": ["__conversation_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "read_file".to_string(),
                description: "Read the text content of an attached file. Use this to examine documents, chat exports, or notes shared by the user.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_id": {"type": "string", "description": "UUID of the file to read"}
                    },
                    "required": ["file_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "ingest_chat_export".to_string(),
                description: "Queue an attached chat export for background learning. Supports auto-detection, WhatsApp/Signal text exports, ChatGPT conversations.json, and generic JSON or speaker-prefixed transcripts. The importer saves only durable facts and relationships, not the raw transcript.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_id": {"type": "string", "description": "UUID of the export file"},
                        "format": {"type": "string", "enum": ["auto", "whatsapp", "signal", "chatgpt", "generic"], "description": "Export format; auto is recommended"}
                    },
                    "required": ["file_id"],
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        match tool_name {
            "list_files" => {
                #[derive(Deserialize)]
                struct Args {
                    #[serde(rename = "__conversation_id")]
                    conversation_id: Uuid,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let files = self
                    .db
                    .list_files_for_conversation(args.conversation_id)
                    .await?;
                Ok(serde_json::to_string_pretty(&files)?)
            }
            "read_file" => {
                #[derive(Deserialize)]
                struct Args {
                    file_id: Uuid,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let record = self
                    .db
                    .get_file_record(&args.file_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("File not found: {}", args.file_id))?;

                let content = tokio::fs::read_to_string(&record.path).await?;
                // Truncation is handled by IntegrationRegistry, but we can do a sanity check here too
                Ok(content)
            }
            "ingest_chat_export" => {
                #[derive(Deserialize)]
                struct Args {
                    file_id: Uuid,
                    #[serde(default)]
                    format: ChatExportFormat,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let import = self
                    .chat_importer
                    .enqueue(args.file_id, args.format)
                    .await?;
                Ok(serde_json::to_string_pretty(&import)?)
            }
            _ => anyhow::bail!("Unknown tool: {tool_name}"),
        }
    }
}
