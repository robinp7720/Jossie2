use futures::StreamExt;
use jossie_core::integration::{ToolCall, ToolDefinition};
use jossie_core::types::{Message, Role};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct LlmClient {
    client: reqwest::Client,
    api_url: String,
    api_key: String,
    model: String,
}

#[derive(Debug, Clone, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAITool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    stream: bool,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAIToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAITool {
    r#type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAIFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAIToolCall {
    id: String,
    r#type: String,
    function: OpenAIFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionMessage,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChunk {
    choices: Vec<ChatCompletionChunkChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChunkChoice {
    delta: ChatCompletionChunkDelta,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChunkDelta {
    content: Option<String>,
    tool_calls: Option<Vec<ChatCompletionChunkToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChunkToolCall {
    index: i32,
    id: Option<String>,
    function: Option<ChatCompletionChunkFunction>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionChunkFunction {
    name: Option<String>,
    arguments: Option<String>,
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

    fn build_messages(messages: &[Message]) -> Vec<OpenAIMessage> {
        let mut items = Vec::new();

        for m in messages {
            match m.role {
                Role::Tool => {
                    items.push(OpenAIMessage {
                        role: "tool".to_string(),
                        name: None,
                        content: Some(m.content.clone()),
                        tool_calls: None,
                        tool_call_id: m.tool_call_id.clone(),
                    });
                }
                Role::Assistant => {
                    let tool_calls = if let Some(tc_val) = &m.tool_calls {
                        if let Ok(flat_calls) =
                            serde_json::from_value::<Vec<ToolCall>>(tc_val.clone())
                        {
                            if flat_calls.is_empty() {
                                None
                            } else {
                                Some(
                                    flat_calls
                                        .into_iter()
                                        .map(|c| OpenAIToolCall {
                                            id: c.id,
                                            r#type: "function".to_string(),
                                            function: OpenAIFunctionCall {
                                                name: c.name,
                                                arguments: c.arguments,
                                            },
                                        })
                                        .collect(),
                                )
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    items.push(OpenAIMessage {
                        role: "assistant".to_string(),
                        name: m.name.clone(),
                        content: if m.content.is_empty() && tool_calls.is_some() {
                            None
                        } else {
                            Some(m.content.clone())
                        },
                        tool_calls,
                        tool_call_id: None,
                    });
                }
                _ => {
                    items.push(OpenAIMessage {
                        role: m.role.to_string().to_lowercase(),
                        name: m.name.clone(),
                        content: Some(m.content.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
            }
        }
        items
    }

    fn build_tools(tools: &[ToolDefinition]) -> Option<Vec<OpenAITool>> {
        if tools.is_empty() {
            return None;
        }
        let built = tools
            .iter()
            .map(|t| OpenAITool {
                r#type: "function".to_string(),
                function: OpenAIFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect();
        Some(built)
    }

    /// Non-streaming completion. Returns content and optional tool calls.
    pub async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<(String, Vec<ToolCall>)> {
        let req = ChatCompletionRequest {
            model: self.model.clone(),
            messages: Self::build_messages(messages),
            tools: Self::build_tools(tools),
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some(serde_json::Value::String("auto".to_string()))
            },
            stream: false,
        };

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.api_url))
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM API error {status}: {body}");
        }

        let response: ChatCompletionResponse = resp.json().await?;
        if let Some(choice) = response.choices.first() {
            let content = choice.message.content.clone().unwrap_or_default();
            let mut tool_calls = Vec::new();
            if let Some(calls) = &choice.message.tool_calls {
                for c in calls {
                    tool_calls.push(ToolCall {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        arguments: c.function.arguments.clone(),
                    });
                }
            }
            Ok((content, tool_calls))
        } else {
            Ok((String::new(), Vec::new()))
        }
    }

    /// Streaming completion. Sends events to the channel.
    pub async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
    ) -> anyhow::Result<()> {
        let req = ChatCompletionRequest {
            model: self.model.clone(),
            messages: Self::build_messages(messages),
            tools: Self::build_tools(tools),
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some(serde_json::Value::String("auto".to_string()))
            },
            stream: true,
        };

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.api_url))
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await?;

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

                let Ok(chunk_data) = serde_json::from_str::<ChatCompletionChunk>(data) else {
                    continue;
                };

                for choice in chunk_data.choices {
                    if let Some(content) = choice.delta.content {
                        if !content.is_empty() {
                            let _ = tx.send(StreamEvent::Delta(content)).await;
                        }
                    }

                    if let Some(tool_calls) = choice.delta.tool_calls {
                        for tc in tool_calls {
                            let entry = pending_calls.entry(tc.index).or_default();
                            if let Some(id) = tc.id {
                                entry.id = Some(id);
                            }
                            if let Some(func) = tc.function {
                                if let Some(name) = func.name {
                                    entry.name = Some(name);
                                }
                                if let Some(args) = func.arguments {
                                    entry.arguments.push_str(&args);
                                }
                            }
                        }
                    }
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
