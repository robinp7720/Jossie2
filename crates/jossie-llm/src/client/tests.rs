#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use uuid::Uuid;

    #[derive(Clone)]
    struct RateLimitTestState {
        attempts: Arc<AtomicUsize>,
        failures_before_success: usize,
    }

    async fn rate_limited_responses_handler(
        axum::extract::State(state): axum::extract::State<RateLimitTestState>,
        axum::Json(request): axum::Json<Value>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;

        let attempt = state.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt < state.failures_before_success {
            return (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::RETRY_AFTER, "0")],
                axum::Json(json!({
                    "error": {
                        "message": "try again later",
                        "code": "rate_limit_exceeded"
                    }
                })),
            )
                .into_response();
        }

        if request["stream"] == true {
            return (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                concat!(
                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"recovered\"}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_retry\",\"usage\":{}}}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
                .into_response();
        }

        axum::Json(json!({
            "id": "resp_retry",
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "recovered"}]
            }]
        }))
        .into_response()
    }

    async fn spawn_rate_limited_responses_server(
        failures_before_success: usize,
    ) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        use axum::{Router, routing::post};

        let attempts = Arc::new(AtomicUsize::new(0));
        let state = RateLimitTestState {
            attempts: attempts.clone(),
            failures_before_success,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/responses", post(rate_limited_responses_handler))
                    .with_state(state),
            )
            .await
            .unwrap();
        });
        (address, attempts)
    }

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

    #[test]
    fn rate_limit_delay_honors_retry_after_and_caps_excessive_values() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "17".parse().unwrap());
        assert_eq!(
            rate_limit_retry_delay(&headers, 0),
            std::time::Duration::from_secs(17)
        );

        headers.insert(reqwest::header::RETRY_AFTER, "120".parse().unwrap());
        assert_eq!(
            rate_limit_retry_delay(&headers, 0),
            std::time::Duration::from_secs(MAX_RETRY_DELAY_SECS)
        );
    }

    #[tokio::test]
    async fn non_streaming_completion_retries_rate_limits_transparently() {
        let (address, attempts) = spawn_rate_limited_responses_server(2).await;
        let client = LlmClient::new(&format!("http://{address}/v1"), "test", "model");

        let output = client
            .complete(&[make_message(Role::User, "hello")], &[])
            .await
            .unwrap();

        assert_eq!(output.content, "recovered");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn streaming_completion_retries_before_emitting_events() {
        let (address, attempts) = spawn_rate_limited_responses_server(2).await;
        let client = LlmClient::new(&format!("http://{address}/v1"), "test", "model");
        let (tx, mut rx) = mpsc::channel(8);

        client
            .complete_stream(&[make_message(Role::User, "hello")], &[], tx)
            .await
            .unwrap();

        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(matches!(rx.recv().await, Some(StreamEvent::Delta(text)) if text == "recovered"));
        assert!(matches!(
            rx.recv().await,
            Some(StreamEvent::Completed { response_id, .. })
                if response_id.as_deref() == Some("resp_retry")
        ));
        assert!(rx.recv().await.is_none());
    }
}
