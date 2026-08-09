impl LlmClient {
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

        let resp = self.send_responses_request(&req).await?;

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

        let resp = match self.send_responses_request(&req).await {
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
