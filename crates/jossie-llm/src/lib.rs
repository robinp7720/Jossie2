use futures::StreamExt;
use jossie_core::integration::{ToolCall, ToolDefinition};
use jossie_core::types::{Message, Role};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct LlmClient {
    client: reqwest::Client,
    api_url: String,
    api_key: String,
    model: String,
    reasoning_effort: Option<String>,
    enable_web_search: bool,
    service_tier: Option<String>,
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
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum ResponseInputItem {
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
    content: Vec<ResponseInputText>,
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
    effort: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output: Vec<ResponseOutputItem>,
    #[allow(dead_code)]
    status: Option<String>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseError {
    message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum ResponseOutputItem {
    #[serde(rename = "message")]
    Message(ResponseOutputMessage),
    #[serde(rename = "function_call")]
    FunctionCall(ResponseFunctionCall),
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseOutputMessage {
    #[serde(default)]
    content: Vec<ResponseOutputContent>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum ResponseOutputContent {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(rename = "refusal")]
    Refusal { refusal: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseFunctionCall {
    call_id: String,
    name: String,
    arguments: String,
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
            reasoning_effort: None,
            enable_web_search: false,
            service_tier: Some("flex".to_string()),
        }
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }

    pub fn set_enable_web_search(&mut self, enabled: bool) {
        self.enable_web_search = enabled;
    }

    pub fn set_service_tier(&mut self, service_tier: Option<String>) {
        self.service_tier = service_tier;
    }

    fn build_input(messages: &[Message]) -> Vec<ResponseInputItem> {
        let mut items = Vec::new();

        for message in messages {
            match message.role {
                Role::System | Role::User => {
                    items.push(ResponseInputItem::InputMessage(ResponseInputMessage {
                        item_type: "message",
                        role: message.role.to_string(),
                        content: vec![ResponseInputText {
                            item_type: "input_text",
                            text: message.content.clone(),
                        }],
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
    ) -> ResponsesRequest {
        let built_tools = self.build_tools(tools);
        let has_tools = built_tools.is_some();

        ResponsesRequest {
            model: self.model.clone(),
            input: Self::build_input(messages),
            tools: built_tools,
            tool_choice: if has_tools {
                Some(Value::String("auto".to_string()))
            } else {
                None
            },
            stream,
            reasoning: self
                .reasoning_effort
                .clone()
                .map(|effort| ResponseReasoningConfig { effort }),
            service_tier: self.service_tier.clone(),
        }
    }

    /// Non-streaming completion. Returns content and optional tool calls.
    pub async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<(String, Vec<ToolCall>)> {
        let req = self.build_request(messages, tools, false);

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
        collect_response_output(response)
    }

    /// Streaming completion. Sends events to the channel.
    pub async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
    ) -> anyhow::Result<()> {
        let req = self.build_request(messages, tools, true);

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
        let mut done_received = false;

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

                handle_stream_event(data, &mut pending_calls, &tx).await;
            }

            if done_received {
                break;
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

fn tool_calls_from_message(message: &Message) -> Option<Vec<ToolCall>> {
    let tool_calls = message.tool_calls.as_ref()?;
    let parsed = serde_json::from_value::<Vec<ToolCall>>(tool_calls.clone()).ok()?;
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

fn collect_response_output(response: ResponsesResponse) -> anyhow::Result<(String, Vec<ToolCall>)> {
    if let Some(error) = response.error {
        anyhow::bail!("LLM API error: {}", error.message);
    }

    let mut content = String::new();
    let mut tool_calls = Vec::new();

    for item in response.output {
        match item {
            ResponseOutputItem::Message(message) => {
                for part in message.content {
                    match part {
                        ResponseOutputContent::OutputText { text } => content.push_str(&text),
                        ResponseOutputContent::Refusal { refusal } => content.push_str(&refusal),
                        ResponseOutputContent::Other => {}
                    }
                }
            }
            ResponseOutputItem::FunctionCall(call) => {
                tool_calls.push(ToolCall {
                    id: call.call_id,
                    name: call.name,
                    arguments: call.arguments,
                });
            }
            ResponseOutputItem::Other => {}
        }
    }

    Ok((content, tool_calls))
}

async fn handle_stream_event(
    data: &str,
    pending_calls: &mut HashMap<i32, PendingToolCall>,
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
    use uuid::Uuid;

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

        let items = LlmClient::build_input(&[
            make_message(Role::System, "You are helpful."),
            make_message(Role::User, "What's the weather?"),
            assistant,
            tool,
        ]);

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
    fn build_request_adds_web_search_tool_when_enabled() {
        let mut client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-4.1");
        client.set_enable_web_search(true);

        let request = client.build_request(&[make_message(Role::User, "Latest news?")], &[], false);
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["tool_choice"], "auto");
        assert_eq!(json["tools"][0]["type"], "web_search");
    }

    #[test]
    fn build_request_defaults_to_flex_service_tier() {
        let client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-4.1");

        let request = client.build_request(&[make_message(Role::User, "Hi")], &[], false);
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["service_tier"], "flex");
    }

    #[test]
    fn build_request_can_omit_service_tier() {
        let mut client = LlmClient::new("https://api.openai.com/v1", "test-key", "gpt-4.1");
        client.set_service_tier(None);

        let request = client.build_request(&[make_message(Role::User, "Hi")], &[], false);
        let json = serde_json::to_value(request).unwrap();

        assert!(json.get("service_tier").is_none());
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

        let request = client.build_request(&[make_message(Role::User, "Find it")], &tools, false);
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["name"], "lookup");
        assert_eq!(json["tools"][1]["type"], "web_search");
    }

    #[test]
    fn collect_response_output_extracts_text_and_function_calls() {
        let response = ResponsesResponse {
            output: vec![
                ResponseOutputItem::Message(ResponseOutputMessage {
                    content: vec![
                        ResponseOutputContent::OutputText {
                            text: "Hello ".to_string(),
                        },
                        ResponseOutputContent::Refusal {
                            refusal: "world".to_string(),
                        },
                    ],
                }),
                ResponseOutputItem::FunctionCall(ResponseFunctionCall {
                    call_id: "call_456".to_string(),
                    name: "lookup".to_string(),
                    arguments: "{\"q\":\"test\"}".to_string(),
                }),
            ],
            status: Some("completed".to_string()),
            error: None,
        };

        let (content, tool_calls) = collect_response_output(response).unwrap();
        assert_eq!(content, "Hello world");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_456");
        assert_eq!(tool_calls[0].name, "lookup");
    }
}
