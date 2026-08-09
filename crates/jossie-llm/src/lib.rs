use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures::StreamExt;
use jossie_core::integration::{ToolCall, ToolDefinition};
use jossie_core::types::{Attachment, Message, Role};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct LlmClient {
    client: reqwest::Client,
    api_url: String,
    api_key: String,
    model: String,
    reasoning_effort: Option<String>,
    reasoning_context: Option<String>,
    enable_web_search: bool,
    service_tier: Option<String>,
    transcription_model: Option<String>,
    max_attachment_bytes_per_request: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponseInputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ResponseTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ResponseReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_options: Option<PromptCacheOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<ResponseTextConfig>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum ResponseInputItem {
    Raw(Value),
    InputMessage(ResponseInputMessage),
    AssistantMessage(ResponseAssistantMessage),
    FunctionCall(ResponseFunctionCallInput),
    FunctionCallOutput(ResponseFunctionCallOutputInput),
}

#[derive(Debug, Clone, Serialize)]
struct ResponseInputMessage {
    #[serde(rename = "type")]
    item_type: &'static str,
    role: String,
    content: Vec<ResponseInputContent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum ResponseInputContent {
    Text(ResponseInputText),
    Image(ResponseInputImage),
    File(ResponseInputFile),
}

#[derive(Debug, Clone, Serialize)]
struct ResponseAssistantMessage {
    #[serde(rename = "type")]
    item_type: &'static str,
    role: &'static str,
    content: Vec<ResponseOutputText>,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseInputText {
    #[serde(rename = "type")]
    item_type: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_breakpoint: Option<PromptCacheBreakpoint>,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseInputImage {
    #[serde(rename = "type")]
    item_type: &'static str,
    image_url: String,
    detail: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseInputFile {
    #[serde(rename = "type")]
    item_type: &'static str,
    filename: String,
    file_data: String,
}

#[derive(Debug, Clone, Serialize)]
struct PromptCacheBreakpoint {
    mode: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct PromptCacheOptions {
    mode: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseTextConfig {
    format: ResponseTextFormat,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseTextFormat {
    #[serde(rename = "type")]
    format_type: &'static str,
    name: String,
    strict: bool,
    schema: Value,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseOutputText {
    #[serde(rename = "type")]
    item_type: &'static str,
    text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum ResponseTool {
    Function(ResponseFunctionTool),
    Hosted(ResponseHostedTool),
}

#[derive(Debug, Clone, Serialize)]
struct ResponseFunctionTool {
    #[serde(rename = "type")]
    item_type: &'static str,
    name: String,
    description: String,
    parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseHostedTool {
    #[serde(rename = "type")]
    item_type: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseFunctionCallInput {
    #[serde(rename = "type")]
    item_type: &'static str,
    call_id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseFunctionCallOutputInput {
    #[serde(rename = "type")]
    item_type: &'static str,
    call_id: String,
    output: String,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    output: Vec<Value>,
    #[allow(dead_code)]
    status: Option<String>,
    #[serde(default)]
    error: Option<ResponseError>,
    #[serde(default)]
    usage: Option<ResponseUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseError {
    message: String,
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Delta(String),
    Completed {
        tool_calls: Vec<ToolCall>,
        response_items: Vec<Value>,
        response_id: Option<String>,
        usage: Option<ResponseUsage>,
    },
    Error(String),
}

#[derive(Debug, Clone)]
pub struct LlmOutput {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub response_items: Vec<Value>,
    pub response_id: Option<String>,
    pub usage: Option<ResponseUsage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub input_tokens_details: ResponseInputTokenDetails,
    #[serde(default)]
    pub output_tokens_details: ResponseOutputTokenDetails,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseInputTokenDetails {
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseOutputTokenDetails {
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LlmRequestOptions {
    pub previous_response_id: Option<String>,
    pub prompt_cache_key: Option<String>,
    pub cache_breakpoint_message_index: Option<usize>,
    pub structured_output: Option<StructuredOutputFormat>,
}

#[derive(Debug, Clone)]
pub struct StructuredOutputFormat {
    pub name: String,
    pub schema: Value,
}

enum AttachmentWireKind {
    Image(String),
    File(String),
}

fn attachment_wire_kind(attachment: &Attachment) -> Option<AttachmentWireKind> {
    let mime = attachment
        .mime_type
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let extension = Path::new(&attachment.name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match (mime.as_str(), extension.as_str()) {
        ("image/jpeg", _) | (_, "jpg" | "jpeg") => {
            Some(AttachmentWireKind::Image("image/jpeg".to_string()))
        }
        ("image/png", _) | (_, "png") => Some(AttachmentWireKind::Image("image/png".to_string())),
        ("image/webp", _) | (_, "webp") => {
            Some(AttachmentWireKind::Image("image/webp".to_string()))
        }
        ("image/gif", _) | (_, "gif") => Some(AttachmentWireKind::Image("image/gif".to_string())),
        ("application/pdf", _) | (_, "pdf") => {
            Some(AttachmentWireKind::File("application/pdf".to_string()))
        }
        _ if mime.starts_with("text/")
            || matches!(
                mime.as_str(),
                "application/json" | "application/xml" | "text/csv"
            ) =>
        {
            Some(AttachmentWireKind::File(mime))
        }
        _ if supported_file_extension(&extension) => {
            Some(AttachmentWireKind::File(if mime.is_empty() {
                "application/octet-stream".to_string()
            } else {
                mime
            }))
        }
        _ => None,
    }
}

fn supported_file_extension(extension: &str) -> bool {
    matches!(
        extension,
        "txt"
            | "md"
            | "json"
            | "html"
            | "htm"
            | "xml"
            | "yaml"
            | "yml"
            | "csv"
            | "tsv"
            | "doc"
            | "docx"
            | "rtf"
            | "odt"
            | "ppt"
            | "pptx"
            | "xls"
            | "xlsx"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "java"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "go"
            | "rb"
            | "php"
            | "sh"
            | "sql"
            | "toml"
            | "css"
    )
}

fn select_attachment_ids(messages: &[Message], max_bytes: usize) -> HashSet<uuid::Uuid> {
    let mut remaining = max_bytes;
    let mut selected = HashSet::new();
    for attachment in messages
        .iter()
        .rev()
        .filter_map(|message| message.attachments.as_ref())
        .flat_map(|attachments| attachments.iter().rev())
    {
        let Some(data) = attachment.data.as_ref() else {
            continue;
        };
        if attachment_wire_kind(attachment).is_none() || data.len() > remaining {
            continue;
        }
        selected.insert(attachment.id);
        remaining -= data.len();
    }
    selected
}

fn attachment_input(attachment: &Attachment) -> Option<ResponseInputContent> {
    let data = attachment.data.as_ref()?;
    let encoded = BASE64_STANDARD.encode(data.as_ref());
    match attachment_wire_kind(attachment)? {
        AttachmentWireKind::Image(mime) => Some(ResponseInputContent::Image(ResponseInputImage {
            item_type: "input_image",
            image_url: format!("data:{mime};base64,{encoded}"),
            detail: "auto",
        })),
        AttachmentWireKind::File(mime) => Some(ResponseInputContent::File(ResponseInputFile {
            item_type: "input_file",
            filename: attachment.name.clone(),
            file_data: format!("data:{mime};base64,{encoded}"),
        })),
    }
}

include!("client/config.rs");
include!("client/request.rs");
include!("client/retry.rs");
include!("client/completion.rs");
include!("client/tests.rs");
