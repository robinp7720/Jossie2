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

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListFilesArgs {
    /// UUID of the conversation (autofilled).
    #[serde(rename = "__conversation_id")]
    conversation_id: Uuid,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    /// UUID of the file to read.
    file_id: Uuid,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IngestChatExportArgs {
    /// UUID of the export file.
    file_id: Uuid,
    /// Export format; auto is recommended.
    #[serde(default)]
    format: ChatExportFormat,
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
            ToolDefinition::for_args::<ListFilesArgs>(
                "list_files",
                "List files attached to the current conversation.",
            ),
            ToolDefinition::for_args::<ReadFileArgs>(
                "read_file",
                "Read the text content of an attached file. Use this to examine documents, chat exports, or notes shared by the user.",
            ),
            ToolDefinition::for_args::<IngestChatExportArgs>(
                "ingest_chat_export",
                "Queue an attached chat export for background learning. Supports auto-detection, WhatsApp/Signal text exports, ChatGPT conversations.json, and generic JSON or speaker-prefixed transcripts. The importer saves only durable facts and relationships, not the raw transcript.",
            ),
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        match tool_name {
            "list_files" => {
                let args: ListFilesArgs = serde_json::from_str(arguments)?;
                let files = self
                    .db
                    .list_files_for_conversation(args.conversation_id)
                    .await?;
                Ok(serde_json::to_string_pretty(&files)?)
            }
            "read_file" => {
                let args: ReadFileArgs = serde_json::from_str(arguments)?;
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
                let args: IngestChatExportArgs = serde_json::from_str(arguments)?;
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
