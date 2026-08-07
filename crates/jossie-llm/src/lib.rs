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

impl LlmClient {
    pub fn new(api_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_url: api_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            reasoning_effort: None,
            reasoning_context: None,
            enable_web_search: false,
            service_tier: Some("flex".to_string()),
            transcription_model: Some("gpt-transcribe".to_string()),
            max_attachment_bytes_per_request: 25 * 1024 * 1024,
        }
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }

    pub fn set_reasoning_context(&mut self, context: Option<String>) {
        self.reasoning_context = context;
    }

    pub fn set_enable_web_search(&mut self, enabled: bool) {
        self.enable_web_search = enabled;
    }

    pub fn set_service_tier(&mut self, service_tier: Option<String>) {
        self.service_tier = service_tier;
    }

    pub fn set_transcription_model(&mut self, transcription_model: Option<String>) {
        self.transcription_model = transcription_model.filter(|model| !model.trim().is_empty());
    }

    pub fn set_max_attachment_bytes_per_request(&mut self, max_bytes: usize) {
        self.max_attachment_bytes_per_request = max_bytes;
    }

    pub fn transcription_is_configured(&self) -> bool {
        self.transcription_model.is_some()
    }

    pub async fn transcribe_file(
        &self,
        path: &Path,
        filename: &str,
        mime_type: &str,
    ) -> anyhow::Result<String> {
        let model = self
            .transcription_model
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Audio transcription is disabled"))?;
        let bytes = tokio::fs::read(path).await?;
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(mime_type)?;
        let form = reqwest::multipart::Form::new()
            .text("model", model.to_string())
            .part("file", part);
        let response = self
            .client
            .post(format!("{}/audio/transcriptions", self.api_url))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Transcription API error {status}: {body}");
        }
        let value: Value = response.json().await?;
        value
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Transcription response did not contain text"))
    }

    fn build_input(
        &self,
        messages: &[Message],
        cache_breakpoint_message_index: Option<usize>,
    ) -> Vec<ResponseInputItem> {
        let mut items = Vec::new();
        let selected_attachments =
            select_attachment_ids(messages, self.max_attachment_bytes_per_request);

        for (message_index, message) in messages.iter().enumerate() {
            if message.role == Role::Assistant
                && let Some(response_items) = &message.response_items
                && !response_items.is_empty()
            {
                items.extend(response_items.iter().cloned().map(ResponseInputItem::Raw));
                continue;
            }

            match message.role {
                Role::System | Role::User => {
                    let mut content = vec![ResponseInputContent::Text(ResponseInputText {
                        item_type: "input_text",
                        text: message.content.clone(),
                        prompt_cache_breakpoint: (cache_breakpoint_message_index
                            == Some(message_index))
                        .then_some(PromptCacheBreakpoint { mode: "explicit" }),
                    })];
                    if message.role == Role::User
                        && let Some(attachments) = &message.attachments
                    {
                        for attachment in attachments {
                            if !selected_attachments.contains(&attachment.id) {
                                continue;
                            }
                            if let Some(input) = attachment_input(attachment) {
                                content.push(input);
                            }
                        }
                    }
                    items.push(ResponseInputItem::InputMessage(ResponseInputMessage {
                        item_type: "message",
                        role: message.role.to_string(),
                        content,
                    }));
                }
                Role::Assistant => {
                    if !message.content.is_empty() {
                        items.push(ResponseInputItem::AssistantMessage(
                            ResponseAssistantMessage {
                                item_type: "message",
                                role: "assistant",
                                content: vec![ResponseOutputText {
                                    item_type: "output_text",
                                    text: message.content.clone(),
                                }],
                            },
                        ));
                    }

                    if let Some(tool_calls) = tool_calls_from_message(message) {
                        for call in tool_calls {
                            items.push(ResponseInputItem::FunctionCall(
                                ResponseFunctionCallInput {
                                    item_type: "function_call",
                                    call_id: call.id,
                                    name: call.name,
                                    arguments: call.arguments,
                                },
                            ));
                        }
                    }
                }
                Role::Tool => {
                    if let Some(call_id) = &message.tool_call_id {
                        items.push(ResponseInputItem::FunctionCallOutput(
                            ResponseFunctionCallOutputInput {
                                item_type: "function_call_output",
                                call_id: call_id.clone(),
                                output: message.content.clone(),
                            },
                        ));
                    }
                }
            }
        }

        items
    }

    fn build_tools(&self, tools: &[ToolDefinition]) -> Option<Vec<ResponseTool>> {
        let mut built: Vec<ResponseTool> = tools
            .iter()
            .map(|tool| {
                ResponseTool::Function(ResponseFunctionTool {
                    item_type: "function",
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                    strict: None,
                })
            })
            .collect();

        if self.enable_web_search {
            built.push(ResponseTool::Hosted(ResponseHostedTool {
                item_type: "web_search",
            }));
        }

        if built.is_empty() { None } else { Some(built) }
    }

    fn build_request(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        stream: bool,
        options: &LlmRequestOptions,
    ) -> ResponsesRequest {
        let built_tools = self.build_tools(tools);
        let has_tools = built_tools.is_some();

        ResponsesRequest {
            model: self.model.clone(),
            input: self.build_input(messages, options.cache_breakpoint_message_index),
            tools: built_tools,
            tool_choice: if has_tools {
                Some(Value::String("auto".to_string()))
            } else {
                None
            },
            stream,
            reasoning: if self.reasoning_effort.is_some() || self.reasoning_context.is_some() {
                Some(ResponseReasoningConfig {
                    effort: self.reasoning_effort.clone(),
                    context: self.reasoning_context.clone(),
                })
            } else {
                None
            },
            service_tier: self.service_tier.clone(),
            previous_response_id: options.previous_response_id.clone(),
            prompt_cache_key: options.prompt_cache_key.clone(),
            prompt_cache_options: options
                .cache_breakpoint_message_index
                .map(|_| PromptCacheOptions { mode: "explicit" }),
            store: options.previous_response_id.as_ref().map(|_| true),
            text: options
                .structured_output
                .as_ref()
                .map(|format| ResponseTextConfig {
                    format: ResponseTextFormat {
                        format_type: "json_schema",
                        name: format.name.clone(),
                        strict: true,
                        schema: format.schema.clone(),
                    },
                }),
        }
    }

    /// Non-streaming completion. Returns content and optional tool calls.
    pub async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmOutput> {
        self.complete_with_options(messages, tools, &LlmRequestOptions::default())
            .await
    }

    pub async fn complete_with_options(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &LlmRequestOptions,
    ) -> anyhow::Result<LlmOutput> {
        let req = self.build_request(messages, tools, false, options);

        let resp = self
            .client
            .post(format!("{}/responses", self.api_url))
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error {status}: {body}");
        }

        let response: ResponsesResponse = resp.json().await?;
        let output = collect_response_output(response)?;
        trace_usage("non_streaming", output.usage.as_ref());
        Ok(output)
    }

    /// Streaming completion. Sends events to the channel.
    pub async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
    ) -> anyhow::Result<()> {
        self.complete_stream_with_options(messages, tools, &LlmRequestOptions::default(), tx)
            .await
    }

    pub async fn complete_stream_with_options(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &LlmRequestOptions,
        tx: mpsc::Sender<StreamEvent>,
    ) -> anyhow::Result<()> {
        let req = self.build_request(messages, tools, true, options);

        let resp = match self
            .client
            .post(format!("{}/responses", self.api_url))
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let _ = tx
                    .send(StreamEvent::Error(format!(
                        "LLM request failed before streaming started: {e}"
                    )))
                    .await;
                return Ok(());
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let _ = tx
                .send(StreamEvent::Error(format!(
                    "LLM API error {status}: {body}"
                )))
                .await;
            return Ok(());
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut pending_calls: HashMap<i32, PendingToolCall> = HashMap::new();
        let mut completed_items: HashMap<i32, Value> = HashMap::new();
        let mut done_received = false;
        let mut response_metadata = StreamResponseMetadata::default();

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    let _ = tx
                        .send(StreamEvent::Error(format!(
                            "LLM stream transport error: {e}"
                        )))
                        .await;
                    return Ok(());
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || !line.starts_with("data: ") {
                    continue;
                }

                let data = &line[6..];
                if data == "[DONE]" {
                    done_received = true;
                    break;
                }

                handle_stream_event(
                    data,
                    &mut pending_calls,
                    &mut completed_items,
                    &mut response_metadata,
                    &tx,
                )
                .await;
            }

            if done_received {
                break;
            }
        }

        let calls = collect_tool_calls(pending_calls);
        let response_items = collect_response_items(completed_items);
        trace_usage("streaming", response_metadata.usage.as_ref());
        let _ = tx
            .send(StreamEvent::Completed {
                tool_calls: calls,
                response_items,
                response_id: response_metadata.response_id,
                usage: response_metadata.usage,
            })
            .await;
        Ok(())
    }
}

fn tool_calls_from_message(message: &Message) -> Option<Vec<ToolCall>> {
    let tool_calls = message.tool_calls.as_ref()?;
    let parsed = serde_json::from_value::<Vec<ToolCall>>(tool_calls.clone()).ok()?;
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

fn collect_response_output(response: ResponsesResponse) -> anyhow::Result<LlmOutput> {
    if let Some(error) = response.error {
        anyhow::bail!("LLM API error: {}", error.message);
    }

    let mut content = String::new();
    let mut tool_calls = Vec::new();

    for item in &response.output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    content.push_str(text);
                                }
                            }
                            Some("refusal") => {
                                if let Some(refusal) = part.get("refusal").and_then(Value::as_str) {
                                    content.push_str(refusal);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("function_call") => {
                let required = |field: &str| {
                    item.get(field).and_then(Value::as_str).ok_or_else(|| {
                        anyhow::anyhow!("Responses function_call is missing {field}")
                    })
                };
                tool_calls.push(ToolCall {
                    id: required("call_id")?.to_string(),
                    name: required("name")?.to_string(),
                    arguments: required("arguments")?.to_string(),
                });
            }
            _ => {}
        }
    }

    Ok(LlmOutput {
        content,
        tool_calls,
        response_items: response.output,
        response_id: response.id,
        usage: response.usage,
    })
}

fn trace_usage(kind: &str, usage: Option<&ResponseUsage>) {
    if let Some(usage) = usage {
        tracing::info!(
            request_kind = kind,
            input_tokens = usage.input_tokens,
            cached_tokens = usage.input_tokens_details.cached_tokens,
            cache_write_tokens = usage.input_tokens_details.cache_write_tokens,
            output_tokens = usage.output_tokens,
            reasoning_tokens = usage.output_tokens_details.reasoning_tokens,
            total_tokens = usage.total_tokens,
            "LLM token usage"
        );
    }
}

#[derive(Default)]
struct StreamResponseMetadata {
    response_id: Option<String>,
    usage: Option<ResponseUsage>,
}

async fn handle_stream_event(
    data: &str,
    pending_calls: &mut HashMap<i32, PendingToolCall>,
    completed_items: &mut HashMap<i32, Value>,
    response_metadata: &mut StreamResponseMetadata,
    tx: &mpsc::Sender<StreamEvent>,
) {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };

    let Some(event_type) = value.get("type").and_then(Value::as_str) else {
        return;
    };

    match event_type {
        "response.output_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                if !delta.is_empty() {
                    let _ = tx.send(StreamEvent::Delta(delta.to_string())).await;
                }
            }
        }
        "response.output_item.added" => {
            let Some(index) = value.get("output_index").and_then(Value::as_i64) else {
                return;
            };
            let Some(item) = value.get("item") else {
                return;
            };
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return;
            }

            let entry = pending_calls.entry(index as i32).or_default();
            entry.id = item
                .get("call_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            entry.name = item
                .get("name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
                entry.arguments = arguments.to_string();
            }
        }
        "response.function_call_arguments.delta" => {
            let Some(index) = value.get("output_index").and_then(Value::as_i64) else {
                return;
            };
            let entry = pending_calls.entry(index as i32).or_default();
            if let Some(call_id) = value.get("call_id").and_then(Value::as_str) {
                entry.id = Some(call_id.to_string());
            }
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                entry.arguments.push_str(delta);
            }
        }
        "response.function_call_arguments.done" => {
            let Some(index) = value.get("output_index").and_then(Value::as_i64) else {
                return;
            };
            let entry = pending_calls.entry(index as i32).or_default();
            if let Some(call_id) = value.get("call_id").and_then(Value::as_str) {
                entry.id = Some(call_id.to_string());
            }
            if let Some(name) = value.get("name").and_then(Value::as_str) {
                entry.name = Some(name.to_string());
            }
            if let Some(arguments) = value.get("arguments").and_then(Value::as_str) {
                entry.arguments = arguments.to_string();
            }
        }
        "response.output_item.done" => {
            let Some(index) = value.get("output_index").and_then(Value::as_i64) else {
                return;
            };
            let Some(item) = value.get("item") else {
                return;
            };
            completed_items.insert(index as i32, item.clone());
        }
        "response.completed" => {
            let Some(response) = value.get("response") else {
                return;
            };
            response_metadata.response_id = response
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            response_metadata.usage = response
                .get("usage")
                .cloned()
                .and_then(|usage| serde_json::from_value(usage).ok());
        }
        "error" => {
            let message = value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
                .unwrap_or("unknown streaming error");
            let _ = tx.send(StreamEvent::Error(message.to_string())).await;
        }
        _ => {}
    }
}

fn collect_response_items(mut items: HashMap<i32, Value>) -> Vec<Value> {
    let mut indices: Vec<i32> = items.keys().copied().collect();
    indices.sort_unstable();
    indices
        .into_iter()
        .filter_map(|index| items.remove(&index))
        .collect()
}

#[derive(Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn collect_tool_calls(mut pending: HashMap<i32, PendingToolCall>) -> Vec<ToolCall> {
    let mut indices: Vec<i32> = pending.keys().cloned().collect();
    indices.sort();

    let mut calls = Vec::new();
    for idx in indices {
        if let Some(ptc) = pending.remove(&idx) {
            if let (Some(id), Some(name)) = (ptc.id, ptc.name) {
                calls.push(ToolCall {
                    id,
                    name,
                    arguments: ptc.arguments,
                });
            }
        }
    }
    calls
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    fn test_client() -> LlmClient {
        LlmClient::new("https://example.invalid/v1", "test", "test-model")
    }

    fn make_message(role: Role, content: &str) -> Message {
        Message {
            id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            role,
            content: content.to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            response_items: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn build_input_preserves_assistant_tool_calls_and_outputs() {
        let mut assistant = make_message(Role::Assistant, "Checking...");
        assistant.tool_calls = Some(json!([
            {
                "id": "call_123",
                "name": "weather_lookup",
                "arguments": "{\"city\":\"Berlin\"}"
            }
        ]));

        let mut tool = make_message(Role::Tool, "{\"temp\":12}");
        tool.tool_call_id = Some("call_123".to_string());

        let items = test_client().build_input(
            &[
                make_message(Role::System, "You are helpful."),
                make_message(Role::User, "What's the weather?"),
                assistant,
                tool,
            ],
            None,
        );

        let json = serde_json::to_value(items).unwrap();
        assert_eq!(json[0]["role"], "system");
        assert_eq!(json[0]["content"][0]["type"], "input_text");
        assert_eq!(json[1]["role"], "user");
        assert_eq!(json[2]["role"], "assistant");
        assert_eq!(json[2]["content"][0]["type"], "output_text");
        assert_eq!(json[3]["type"], "function_call");
        assert_eq!(json[3]["call_id"], "call_123");
        assert_eq!(json[4]["type"], "function_call_output");
        assert_eq!(json[4]["call_id"], "call_123");
    }

    #[test]
    fn build_input_replays_exact_response_items_before_tool_outputs() {
        let reasoning = json!({
            "type": "reasoning",
            "id": "rs_123",
            "summary": []
        });
        let function_call = json!({
            "type": "function_call",
            "id": "fc_123",
            "call_id": "call_123",
            "name": "weather_lookup",
            "arguments": "{\"city\":\"Berlin\"}",
            "status": "completed"
        });
        let assistant = make_message(Role::Assistant, "Checking...")
            .with_response_items(vec![reasoning.clone(), function_call.clone()]);
        let mut tool = make_message(Role::Tool, "{\"temp\":12}");
        tool.tool_call_id = Some("call_123".to_string());

        let json =
            serde_json::to_value(test_client().build_input(&[assistant, tool], None)).unwrap();

        assert_eq!(json[0], reasoning);
        assert_eq!(json[1], function_call);
        assert_eq!(json[2]["type"], "function_call_output");
        assert_eq!(json[2]["call_id"], "call_123");
    }

    #[test]
    fn build_input_serializes_images_and_files_without_exposing_payload_metadata() {
        let mut user = make_message(Role::User, "Inspect these");
        user.attachments = Some(vec![
            Attachment {
                id: Uuid::new_v4(),
                name: "photo.jpg".to_string(),
                mime_type: Some("image/jpeg".to_string()),
                size: 3,
                data: Some(Arc::from(vec![1_u8, 2, 3])),
            },
            Attachment {
                id: Uuid::new_v4(),
                name: "report.pdf".to_string(),
                mime_type: Some("application/pdf".to_string()),
                size: 3,
                data: Some(Arc::from(vec![4_u8, 5, 6])),
            },
        ]);

        let input = serde_json::to_value(test_client().build_input(&[user], None)).unwrap();
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][1]["type"], "input_image");
        assert!(
            input[0]["content"][1]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
        assert_eq!(input[0]["content"][2]["type"], "input_file");
        assert_eq!(input[0]["content"][2]["filename"], "report.pdf");
    }

    #[test]
    fn attachment_budget_prefers_newest_messages() {
        let attachment = |name: &str| Attachment {
            id: Uuid::new_v4(),
            name: name.to_string(),
            mime_type: Some("image/png".to_string()),
            size: 4,
            data: Some(Arc::from(vec![1_u8; 4])),
        };
        let mut older = make_message(Role::User, "older");
        older.attachments = Some(vec![attachment("older.png")]);
        let mut newer = make_message(Role::User, "newer");
        newer.attachments = Some(vec![attachment("newer.png")]);
        let mut client = test_client();
        client.set_max_attachment_bytes_per_request(4);

        let input = serde_json::to_value(client.build_input(&[older, newer], None)).unwrap();
        assert_eq!(input[0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(input[1]["content"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn transcribe_file_posts_multipart_and_reads_text() {
        use axum::{Json, Router, body::Bytes, routing::post};

        async fn transcribe(body: Bytes) -> Json<Value> {
            let body = String::from_utf8_lossy(&body);
            assert!(body.contains("gpt-transcribe"));
            assert!(body.contains("voice payload"));
            Json(json!({"text": "  hello from voice  "}))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/v1/audio/transcriptions", post(transcribe)),
            )
            .await
            .unwrap();
        });
        let path = std::env::temp_dir().join(format!("jossie-voice-{}.webm", Uuid::new_v4()));
        tokio::fs::write(&path, b"voice payload").await.unwrap();
        let client = LlmClient::new(&format!("http://{address}/v1"), "test", "model");
        let transcript = client
            .transcribe_file(&path, "voice.webm", "audio/webm")
            .await
            .unwrap();
        let _ = tokio::fs::remove_file(path).await;
        assert_eq!(transcript, "hello from voice");
    }

    #[test]
    fn build_request_adds_web_search_tool_when_enabled() {
        let mut client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-4.1");
        client.set_enable_web_search(true);

        let request = client.build_request(
            &[make_message(Role::User, "Latest news?")],
            &[],
            false,
            &LlmRequestOptions::default(),
        );
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["tool_choice"], "auto");
        assert_eq!(json["tools"][0]["type"], "web_search");
    }

    #[test]
    fn build_request_defaults_to_flex_service_tier() {
        let client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-4.1");

        let request = client.build_request(
            &[make_message(Role::User, "Hi")],
            &[],
            false,
            &LlmRequestOptions::default(),
        );
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["service_tier"], "flex");
    }

    #[test]
    fn build_request_can_omit_service_tier() {
        let mut client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-4.1");
        client.set_service_tier(None);

        let request = client.build_request(
            &[make_message(Role::User, "Hi")],
            &[],
            false,
            &LlmRequestOptions::default(),
        );
        let json = serde_json::to_value(request).unwrap();

        assert!(json.get("service_tier").is_none());
    }

    #[test]
    fn build_request_serializes_reasoning_effort_and_context() {
        let mut client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-5.6-sol");
        client.set_reasoning_effort(Some("low".to_string()));
        client.set_reasoning_context(Some("current_turn".to_string()));

        let request = client.build_request(
            &[make_message(Role::User, "Hi")],
            &[],
            false,
            &LlmRequestOptions::default(),
        );
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["reasoning"]["effort"], "low");
        assert_eq!(json["reasoning"]["context"], "current_turn");
    }

    #[test]
    fn build_request_omits_reasoning_when_unconfigured() {
        let client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-4.1");

        let request = client.build_request(
            &[make_message(Role::User, "Hi")],
            &[],
            false,
            &LlmRequestOptions::default(),
        );
        let json = serde_json::to_value(request).unwrap();

        assert!(json.get("reasoning").is_none());
    }

    #[test]
    fn build_request_preserves_function_tools_when_web_search_enabled() {
        let mut client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-4.1");
        client.set_enable_web_search(true);

        let tools = vec![ToolDefinition {
            name: "lookup".to_string(),
            description: "Looks something up".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        }];

        let request = client.build_request(
            &[make_message(Role::User, "Find it")],
            &tools,
            false,
            &LlmRequestOptions::default(),
        );
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["name"], "lookup");
        assert_eq!(json["tools"][1]["type"], "web_search");
    }

    #[test]
    fn build_request_marks_explicit_stable_prefix() {
        let client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-5.6-sol");
        let options = LlmRequestOptions {
            prompt_cache_key: Some("jossie:chat:abc".to_string()),
            cache_breakpoint_message_index: Some(0),
            ..LlmRequestOptions::default()
        };

        let request = client.build_request(
            &[
                make_message(Role::System, "stable"),
                make_message(Role::System, "dynamic"),
                make_message(Role::User, "hello"),
            ],
            &[],
            false,
            &options,
        );
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["prompt_cache_key"], "jossie:chat:abc");
        assert_eq!(json["prompt_cache_options"]["mode"], "explicit");
        assert_eq!(
            json["input"][0]["content"][0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
        assert!(
            json["input"][1]["content"][0]
                .get("prompt_cache_breakpoint")
                .is_none()
        );
    }

    #[test]
    fn continuation_request_uses_previous_response_id_without_cache_write() {
        let client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-5.6-sol");
        let options = LlmRequestOptions {
            previous_response_id: Some("resp_previous".to_string()),
            ..LlmRequestOptions::default()
        };
        let request = client.build_request(
            &[make_message(Role::Tool, "result").with_tool_call_id("call_1".to_string())],
            &[],
            false,
            &options,
        );
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["previous_response_id"], "resp_previous");
        assert_eq!(json["store"], true);
        assert!(json.get("prompt_cache_options").is_none());
    }

    #[test]
    fn build_request_serializes_structured_output_schema() {
        let client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-5.6-luna");
        let request = client.build_request(
            &[make_message(Role::User, "classify")],
            &[],
            false,
            &LlmRequestOptions {
                structured_output: Some(StructuredOutputFormat {
                    name: "classification".to_string(),
                    schema: json!({
                        "type": "object",
                        "properties": {"label": {"type": "string"}},
                        "required": ["label"],
                        "additionalProperties": false
                    }),
                }),
                ..LlmRequestOptions::default()
            },
        );
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["text"]["format"]["type"], "json_schema");
        assert_eq!(json["text"]["format"]["name"], "classification");
        assert_eq!(json["text"]["format"]["strict"], true);
    }

    #[test]
    fn collect_response_output_extracts_text_and_function_calls() {
        let reasoning = json!({
            "type": "reasoning",
            "id": "rs_456",
            "summary": []
        });
        let response = ResponsesResponse {
            id: Some("resp_456".to_string()),
            output: vec![
                reasoning.clone(),
                json!({
                    "type": "message",
                    "content": [
                        {"type": "output_text", "text": "Hello "},
                        {"type": "refusal", "refusal": "world"}
                    ]
                }),
                json!({
                    "type": "function_call",
                    "call_id": "call_456",
                    "name": "lookup",
                    "arguments": "{\"q\":\"test\"}"
                }),
            ],
            status: Some("completed".to_string()),
            error: None,
            usage: Some(ResponseUsage {
                input_tokens: 10,
                output_tokens: 4,
                total_tokens: 14,
                ..ResponseUsage::default()
            }),
        };

        let output = collect_response_output(response).unwrap();
        assert_eq!(output.content, "Hello world");
        assert_eq!(output.tool_calls.len(), 1);
        assert_eq!(output.tool_calls[0].id, "call_456");
        assert_eq!(output.tool_calls[0].name, "lookup");
        assert_eq!(output.response_items.len(), 3);
        assert_eq!(output.response_items[0], reasoning);
        assert_eq!(output.response_id.as_deref(), Some("resp_456"));
        assert_eq!(output.usage.unwrap().total_tokens, 14);
    }

    #[tokio::test]
    async fn stream_events_preserve_completed_items_in_output_order() {
        let (tx, _rx) = mpsc::channel(1);
        let mut pending_calls = HashMap::new();
        let mut completed_items = HashMap::new();
        let mut response_metadata = StreamResponseMetadata::default();
        let second = json!({"type": "function_call", "call_id": "call_1"});
        let first = json!({"type": "reasoning", "id": "rs_1", "summary": []});

        handle_stream_event(
            &json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": second
            })
            .to_string(),
            &mut pending_calls,
            &mut completed_items,
            &mut response_metadata,
            &tx,
        )
        .await;
        handle_stream_event(
            &json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": first
            })
            .to_string(),
            &mut pending_calls,
            &mut completed_items,
            &mut response_metadata,
            &tx,
        )
        .await;
        handle_stream_event(
            &json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_stream",
                    "usage": {
                        "input_tokens": 20,
                        "input_tokens_details": {"cached_tokens": 10, "cache_write_tokens": 2},
                        "output_tokens": 5,
                        "output_tokens_details": {"reasoning_tokens": 1},
                        "total_tokens": 25
                    }
                }
            })
            .to_string(),
            &mut pending_calls,
            &mut completed_items,
            &mut response_metadata,
            &tx,
        )
        .await;

        let items = collect_response_items(completed_items);
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(
            response_metadata.response_id.as_deref(),
            Some("resp_stream")
        );
        assert_eq!(
            response_metadata
                .usage
                .unwrap()
                .input_tokens_details
                .cached_tokens,
            10
        );
    }
}
