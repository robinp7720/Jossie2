use futures::StreamExt;
use jossie_core::integration::{ToolCall, ToolDefinition};
use jossie_core::types::{Message, Role};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct LlmClient {
    client: reqwest::Client,
    api_url: String,
    api_key: String,
    model: String,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseRequest {
    model: String,
    input: Vec<InputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ResponseTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<Vec<String>>,
    stream: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum InputItem {
    #[serde(rename = "message")]
    Message { role: String, content: Vec<InputContent> },
    #[serde(rename = "function_call")]
    FunctionCall { call_id: String, name: String, arguments: String },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput { call_id: String, output: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum InputContent {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "output_text")]
    OutputText { text: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum ResponseTool {
    #[serde(rename = "function")]
    Function {
        name: String,
        description: String,
        parameters: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "web_search")]
    WebSearch {},
    #[serde(rename = "code_interpreter")]
    CodeInterpreter { container: CodeInterpreterContainer },
}

#[derive(Debug, Clone, Serialize)]
struct CodeInterpreterContainer {
    r#type: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseBody {
    #[serde(default)]
    output: Vec<ResponseOutputItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum ResponseOutputItem {
    #[serde(rename = "message")]
    Message { #[serde(default)] content: Vec<ResponseContentPart> },
    #[serde(rename = "web_search_call")]
    WebSearchCall { #[serde(default)] action: Option<WebSearchAction> },
    #[serde(rename = "function_call")]
    FunctionCall {
        #[serde(default)]
        call_id: Option<String>,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        arguments: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum ResponseContentPart {
    #[serde(rename = "output_text")]
    OutputText {
        text: String,
        #[serde(default)]
        annotations: Vec<ResponseAnnotation>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum ResponseAnnotation {
    #[serde(rename = "url_citation")]
    UrlCitation {
        url: String,
        #[serde(default)]
        title: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
struct WebSearchAction {
    #[serde(default)]
    sources: Vec<WebSearchSource>,
}

#[derive(Debug, Clone, Deserialize)]
struct WebSearchSource {
    url: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Delta(String),
    ToolCalls(Vec<ToolCall>),
    Done,
    Error(String),
}

impl LlmClient {
    pub fn new(api_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_url: api_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }

    fn build_input(messages: &[Message]) -> Vec<InputItem> {
        let mut items = Vec::new();

        for m in messages {
            match m.role {
                Role::Tool => {
                    if let Some(call_id) = &m.tool_call_id {
                        items.push(InputItem::FunctionCallOutput {
                            call_id: call_id.clone(),
                            output: m.content.clone(),
                        });
                    } else if !m.content.is_empty() {
                        items.push(InputItem::Message {
                            role: m.role.to_string(),
                            content: vec![input_content_for_role(&m.role, &m.content)],
                        });
                    }
                }
                _ => {
                    if !m.content.is_empty() {
                        items.push(InputItem::Message {
                            role: m.role.to_string(),
                            content: vec![input_content_for_role(&m.role, &m.content)],
                        });
                    }

                    if let Some(tc_val) = &m.tool_calls {
                        if let Ok(flat_calls) = serde_json::from_value::<Vec<ToolCall>>(tc_val.clone()) {
                            for call in flat_calls {
                                items.push(InputItem::FunctionCall {
                                    call_id: call.id,
                                    name: call.name,
                                    arguments: call.arguments,
                                });
                            }
                        }
                    }
                }
            }
        }

        items
    }

    fn build_tools(tools: &[ToolDefinition]) -> Option<Vec<ResponseTool>> {
        let mut built = Vec::new();

        built.push(ResponseTool::WebSearch {});
        built.push(ResponseTool::CodeInterpreter {
            container: CodeInterpreterContainer { r#type: "auto".to_string() },
        });

        for t in tools {
            built.push(ResponseTool::Function {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
                strict: Some(true),
            });
        }

        if built.is_empty() {
            None
        } else {
            Some(built)
        }
    }

    /// Non-streaming completion. Returns content and optional tool calls.
    pub async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<(String, Vec<ToolCall>)> {
        let req = ResponseRequest {
            model: self.model.clone(),
            input: Self::build_input(messages),
            tools: Self::build_tools(tools),
            tool_choice: Some(serde_json::Value::String("auto".to_string())),
            include: Some(vec!["web_search_call.action.sources".to_string()]),
            stream: false,
        };

        let resp = self.client
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

        let response: ResponseBody = resp.json().await?;
        let mut content = String::new();
        let mut tool_calls = Vec::new();
        let mut sources: Vec<WebSearchSource> = Vec::new();
        let mut annotations: Vec<WebSearchSource> = Vec::new();

        for item in response.output {
            match item {
                ResponseOutputItem::Message { content: parts } => {
                    for part in parts {
                        if let ResponseContentPart::OutputText { text, annotations: ann } = part {
                            content.push_str(&text);
                            for annotation in ann {
                                if let ResponseAnnotation::UrlCitation { url, title } = annotation {
                                    annotations.push(WebSearchSource { url, title });
                                }
                            }
                        }
                    }
                }
                ResponseOutputItem::WebSearchCall { action } => {
                    if let Some(action) = action {
                        sources.extend(action.sources);
                    }
                }
                ResponseOutputItem::FunctionCall { call_id, id, name, arguments } => {
                    let Some(name) = name else { continue };
                    let call_id = call_id.or(id).unwrap_or_default();
                    tool_calls.push(ToolCall {
                        id: call_id,
                        name,
                        arguments: arguments.unwrap_or_default(),
                    });
                }
                ResponseOutputItem::Other => {}
            }
        }

        Ok((content, tool_calls))
    }

    /// Streaming completion. Sends events to the channel.
    pub async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
    ) -> anyhow::Result<()> {
        let req = ResponseRequest {
            model: self.model.clone(),
            input: Self::build_input(messages),
            tools: Self::build_tools(tools),
            tool_choice: Some(serde_json::Value::String("auto".to_string())),
            include: Some(vec!["web_search_call.action.sources".to_string()]),
            stream: true,
        };

        let resp = self.client
            .post(format!("{}/responses", self.api_url))
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let _ = tx.send(StreamEvent::Error(format!("LLM API error {status}: {body}"))).await;
            return Ok(());
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut pending_calls: HashMap<String, PendingToolCall> = HashMap::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || !line.starts_with("data: ") {
                    continue;
                }
                let data = &line[6..];
                if data == "[DONE]" {
                    let calls = collect_tool_calls(pending_calls);
                    if !calls.is_empty() {
                        let _ = tx.send(StreamEvent::ToolCalls(calls)).await;
                    }
                    let _ = tx.send(StreamEvent::Done).await;
                    return Ok(());
                }

                let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };
                let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

                match event_type {
                    "response.output_text.delta" => {
                        if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                            if !delta.is_empty() {
                                let _ = tx.send(StreamEvent::Delta(delta.to_string())).await;
                            }
                        }
                    }
                    "response.output_item.added" => {
                        let item_id = event.get("item_id").and_then(|v| v.as_str()).unwrap_or("");
                        if item_id.is_empty() {
                            continue;
                        }
                        if let Some(item) = event.get("item") {
                            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if item_type == "function_call" {
                                let call_id = item.get("call_id").and_then(|v| v.as_str())
                                    .or_else(|| item.get("id").and_then(|v| v.as_str()))
                                    .map(|s| s.to_string());
                                let name = item.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());
                                let arguments = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let entry = pending_calls.entry(item_id.to_string()).or_default();
                                if entry.call_id.is_none() {
                                    entry.call_id = call_id;
                                }
                                if entry.name.is_none() {
                                    entry.name = name;
                                }
                                if !arguments.is_empty() {
                                    entry.arguments.push_str(&arguments);
                                }
                            }
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        let item_id = event.get("item_id").and_then(|v| v.as_str()).unwrap_or("");
                        if item_id.is_empty() {
                            continue;
                        }
                        if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                            let entry = pending_calls.entry(item_id.to_string()).or_default();
                            entry.arguments.push_str(delta);
                        }
                    }
                    "response.function_call_arguments.done" => {
                        let item_id = event.get("item_id").and_then(|v| v.as_str()).unwrap_or("");
                        if item_id.is_empty() {
                            continue;
                        }
                        let entry = pending_calls.entry(item_id.to_string()).or_default();
                        if let Some(call_id) = event.get("call_id").and_then(|v| v.as_str()) {
                            entry.call_id = Some(call_id.to_string());
                        }
                        if let Some(name) = event.get("name").and_then(|v| v.as_str()) {
                            entry.name = Some(name.to_string());
                        }
                        if let Some(arguments) = event.get("arguments").and_then(|v| v.as_str()) {
                            entry.arguments = arguments.to_string();
                        }
                    }
                    "response.completed" => {
                        if let Some(response_val) = event.get("response") {
                            let sources = sources_from_response_value(response_val);
                            if !sources.is_empty() {
                                let mut suffix = String::new();
                                append_sources(&mut suffix, &sources);
                                let _ = tx.send(StreamEvent::Delta(suffix)).await;
                            }
                        }
                        let calls = collect_tool_calls(pending_calls);
                        if !calls.is_empty() {
                            let _ = tx.send(StreamEvent::ToolCalls(calls)).await;
                        }
                        let _ = tx.send(StreamEvent::Done).await;
                        return Ok(());
                    }
                    "error" => {
                        let message = event.get("message").and_then(|v| v.as_str())
                            .or_else(|| event.get("error").and_then(|e| e.get("message")).and_then(|v| v.as_str()));
                        if let Some(message) = message {
                            let _ = tx.send(StreamEvent::Error(message.to_string())).await;
                        }
                    }
                    _ => {}
                }
            }
        }

        let calls = collect_tool_calls(pending_calls);
        if !calls.is_empty() {
            let _ = tx.send(StreamEvent::ToolCalls(calls)).await;
        }
        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}

fn input_content_for_role(role: &Role, text: &str) -> InputContent {
    match role {
        Role::Assistant | Role::Tool => InputContent::OutputText { text: text.to_string() },
        Role::System | Role::User => InputContent::InputText { text: text.to_string() },
    }
}


#[derive(Default)]
struct PendingToolCall {
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn collect_tool_calls(pending: HashMap<String, PendingToolCall>) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    for (item_id, pending_call) in pending {
        let Some(name) = pending_call.name else { continue };
        let id = pending_call.call_id.unwrap_or(item_id);
        calls.push(ToolCall {
            id,
            name,
            arguments: pending_call.arguments,
        });
    }
    calls
}

fn append_sources(content: &mut String, sources: &[WebSearchSource]) {
    if sources.is_empty() {
        return;
    }
    let mut rendered = Vec::with_capacity(sources.len());
    for source in sources {
        rendered.push(format_source(source));
    }
    let list = join_natural_list(&rendered);
    content.push_str("\n\nFor reference, I checked ");
    content.push_str(&list);
    content.push('.');
}

fn format_source(source: &WebSearchSource) -> String {
    if let Some(title) = &source.title {
        if !title.is_empty() {
            return format!("{title} ({})", source.url);
        }
    }
    source.url.clone()
}

fn join_natural_list(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        2 => format!("{} and {}", items[0], items[1]),
        _ => {
            let mut combined = String::new();
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    if idx + 1 == items.len() {
                        combined.push_str(", and ");
                    } else {
                        combined.push_str(", ");
                    }
                }
                combined.push_str(item);
            }
            combined
        }
    }
}

fn merge_sources(primary: Vec<WebSearchSource>, secondary: Vec<WebSearchSource>) -> Vec<WebSearchSource> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for source in primary.into_iter().chain(secondary) {
        if seen.insert(source.url.clone()) {
            merged.push(source);
        }
    }
    merged
}

fn sources_from_response_value(value: &serde_json::Value) -> Vec<WebSearchSource> {
    let Ok(body) = serde_json::from_value::<ResponseBody>(value.clone()) else {
        return Vec::new();
    };
    sources_from_response_body(body)
}

fn sources_from_response_body(body: ResponseBody) -> Vec<WebSearchSource> {
    let mut sources = Vec::new();
    let mut annotations = Vec::new();

    for item in body.output {
        match item {
            ResponseOutputItem::Message { content: parts } => {
                for part in parts {
                    if let ResponseContentPart::OutputText { annotations: ann, .. } = part {
                        for annotation in ann {
                            if let ResponseAnnotation::UrlCitation { url, title } = annotation {
                                annotations.push(WebSearchSource { url, title });
                            }
                        }
                    }
                }
            }
            ResponseOutputItem::WebSearchCall { action } => {
                if let Some(action) = action {
                    sources.extend(action.sources);
                }
            }
            ResponseOutputItem::FunctionCall { .. } | ResponseOutputItem::Other => {}
        }
    }

    merge_sources(sources, annotations)
}
