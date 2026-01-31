use futures::StreamExt;
use jossie_core::integration::{ToolCall, ToolDefinition};
use jossie_core::types::{Message, Role};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct LlmClient {
    client: reqwest::Client,
    api_url: String,
    api_key: String,
    model: String,
}

#[derive(Debug, Clone, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolSchema>>,
    stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ToolSchema {
    r#type: String,
    function: FunctionSchema,
}

#[derive(Debug, Clone, Serialize)]
struct FunctionSchema {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: Option<ResponseMessage>,
    delta: Option<ResponseMessage>,
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseToolCall {
    id: Option<String>,
    function: Option<ResponseFunction>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseFunction {
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

    fn build_messages(messages: &[Message]) -> Vec<ChatMessage> {
        messages.iter().map(|m| ChatMessage {
            role: m.role.to_string(),
            content: if m.content.is_empty() && m.role != Role::Tool { None } else { Some(m.content.clone()) },
            tool_calls: m.tool_calls.clone(),
            tool_call_id: m.tool_call_id.clone(),
            name: m.name.clone(),
        }).collect()
    }

    fn build_tools(tools: &[ToolDefinition]) -> Option<Vec<ToolSchema>> {
        if tools.is_empty() {
            return None;
        }
        Some(tools.iter().map(|t| ToolSchema {
            r#type: "function".to_string(),
            function: FunctionSchema {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        }).collect())
    }

    /// Non-streaming completion. Returns content and optional tool calls.
    pub async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<(String, Vec<ToolCall>)> {
        let req = ChatRequest {
            model: self.model.clone(),
            messages: Self::build_messages(messages),
            tools: Self::build_tools(tools),
            stream: false,
        };

        let resp = self.client
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

        let chat_resp: ChatResponse = resp.json().await?;
        let choice = chat_resp.choices.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("No choices in response"))?;
        let msg = choice.message.unwrap_or(ResponseMessage { content: None, tool_calls: None });

        let content = msg.content.unwrap_or_default();
        let tool_calls = msg.tool_calls.unwrap_or_default().into_iter().map(|tc| {
            let func = tc.function.unwrap_or(ResponseFunction { name: None, arguments: None });
            ToolCall {
                id: tc.id.unwrap_or_default(),
                name: func.name.unwrap_or_default(),
                arguments: func.arguments.unwrap_or_default(),
            }
        }).collect();

        Ok((content, tool_calls))
    }

    /// Streaming completion. Sends events to the channel.
    pub async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
    ) -> anyhow::Result<()> {
        let req = ChatRequest {
            model: self.model.clone(),
            messages: Self::build_messages(messages),
            tools: Self::build_tools(tools),
            stream: true,
        };

        let resp = self.client
            .post(format!("{}/chat/completions", self.api_url))
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
        let mut accumulated_tool_calls: Vec<AccumulatedToolCall> = Vec::new();

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
                    if !accumulated_tool_calls.is_empty() {
                        let calls: Vec<ToolCall> = accumulated_tool_calls.into_iter().map(|atc| ToolCall {
                            id: atc.id,
                            name: atc.name,
                            arguments: atc.arguments,
                        }).collect();
                        let _ = tx.send(StreamEvent::ToolCalls(calls)).await;
                    }
                    let _ = tx.send(StreamEvent::Done).await;
                    return Ok(());
                }

                if let Ok(resp) = serde_json::from_str::<ChatResponse>(data) {
                    if let Some(choice) = resp.choices.into_iter().next() {
                        if let Some(delta) = choice.delta {
                            if let Some(content) = delta.content {
                                if !content.is_empty() {
                                    let _ = tx.send(StreamEvent::Delta(content)).await;
                                }
                            }
                            if let Some(tcs) = delta.tool_calls {
                                for tc in tcs {
                                    let func = tc.function.unwrap_or(ResponseFunction { name: None, arguments: None });
                                    if let Some(id) = tc.id {
                                        accumulated_tool_calls.push(AccumulatedToolCall {
                                            id,
                                            name: func.name.unwrap_or_default(),
                                            arguments: func.arguments.unwrap_or_default(),
                                        });
                                    } else if let Some(last) = accumulated_tool_calls.last_mut() {
                                        if let Some(args) = func.arguments {
                                            last.arguments.push_str(&args);
                                        }
                                    }
                                }
                            }
                        }
                        if choice.finish_reason.as_deref() == Some("tool_calls") {
                            if !accumulated_tool_calls.is_empty() {
                                let calls: Vec<ToolCall> = accumulated_tool_calls.into_iter().map(|atc| ToolCall {
                                    id: atc.id,
                                    name: atc.name,
                                    arguments: atc.arguments,
                                }).collect();
                                let _ = tx.send(StreamEvent::ToolCalls(calls)).await;
                                accumulated_tool_calls = Vec::new();
                            }
                        }
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}

struct AccumulatedToolCall {
    id: String,
    name: String,
    arguments: String,
}
