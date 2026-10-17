//! Optional anonymous Parallel Search MCP, using the SDK's Streamable HTTP transport.
use std::time::Duration;

use anyhow::{Context, ensure};
use rmcp::{
    ClientLifecycleMode, ClientServiceExt,
    model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::json;

const ENDPOINT: &str = "https://search.parallel.ai/mcp";
const USER_AGENT: &str = concat!(
    "Jossie2/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/robinp7720/Jossie2)"
);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct ParallelSearch {
    endpoint: String,
    timeout: Duration,
}

impl Default for ParallelSearch {
    fn default() -> Self {
        Self {
            endpoint: ENDPOINT.to_string(),
            timeout: SEARCH_TIMEOUT,
        }
    }
}

impl ParallelSearch {
    pub(super) async fn search(&self, query: &str) -> anyhow::Result<String> {
        ensure!(!query.trim().is_empty(), "Search query must not be empty");
        // Dropping the SDK service cancels it, including when the caller cancels this
        // future or the total handshake/discovery/search deadline expires.
        tokio::time::timeout(self.timeout, async {
            let headers = [("user-agent".parse()?, USER_AGENT.parse()?)].into();
            let transport = StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(self.endpoint.clone())
                    .custom_headers(headers)
                    .reinit_on_expired_session(false),
            );
            let client = ClientConfig::new(
                ClientCapabilities::default(),
                Implementation::new("Jossie2", env!("CARGO_PKG_VERSION")),
            )
            .serve_with_lifecycle(transport, ClientLifecycleMode::Initialize)
            .await
            .context("Could not connect to Parallel Search MCP")?;

            let outcome = async {
                let tools = client.list_all_tools().await?;
                ensure!(
                    tools.iter().any(|tool| tool.name == "web_search"),
                    "Parallel Search MCP did not advertise web_search"
                );
                let result = client
                    .call_tool(
                        CallToolRequestParams::new("web_search").with_arguments(
                            json!({"objective": query, "search_queries": [query]})
                                .as_object()
                                .expect("object literal")
                                .clone(),
                        ),
                    )
                    .await?;
                let text = result
                    .content
                    .iter()
                    .filter_map(|content| content.as_text().map(|text| text.text.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                ensure!(
                    result.is_error != Some(true),
                    "Parallel search failed: {text}"
                );
                ensure!(!text.trim().is_empty(), "Parallel search returned no text");
                Ok(format!(
                    "### Search Results\nProvider: Parallel\nQuery: {query}\n\n{text}"
                ))
            }
            .await;
            // Close the MCP service on both successful calls and server errors.
            let _ = client.cancel().await;
            outcome
        })
        .await
        .context("Parallel search timed out")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrowserIntegration, BrowserSearchArgs, BrowserSearchProvider};
    use axum::{
        Router,
        body::Bytes,
        extract::State,
        http::{HeaderMap, Method, StatusCode, Uri},
        response::{IntoResponse, Response},
        routing::any,
    };
    use jossie_core::{IntegrationRegistry, Message, Role, ToolCall, integration::CapabilityGroup};
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

    struct RecordedRequest {
        uri: Uri,
        headers: HeaderMap,
        body: serde_json::Value,
    }

    #[derive(Clone, Default)]
    struct FixtureState {
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        called: Arc<Notify>,
        closed: Arc<Notify>,
        sse: bool,
    }

    struct Fixture {
        endpoint: String,
        state: FixtureState,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn handler(
        State(state): State<FixtureState>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        state.requests.lock().unwrap().push(RecordedRequest {
            uri,
            headers,
            body: request.clone(),
        });
        if method == Method::DELETE {
            state.closed.notify_one();
            return StatusCode::OK.into_response();
        }
        if method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => json!({
                "protocolVersion": request["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture", "version": "1"}
            }),
            "notifications/initialized" | "notifications/cancelled" => {
                return StatusCode::ACCEPTED.into_response();
            }
            "tools/list" => json!({"tools": [{
                "name": "web_search", "inputSchema": {"type": "object"}
            }]}),
            "tools/call" => {
                state.called.notify_one();
                let query = request["params"]["arguments"]["objective"]
                    .as_str()
                    .unwrap();
                if query == "timeout" {
                    std::future::pending::<()>().await;
                }
                if query == "error" {
                    json!({"isError": true, "content": [{"type": "text", "text": "quota exhausted"}]})
                } else if query == "empty" {
                    json!({"content": []})
                } else {
                    json!({"content": [{"type": "text", "text": "Rust documentation\nhttps://www.rust-lang.org/\nThe Rust language home page."}]})
                }
            }
            _ => return StatusCode::BAD_REQUEST.into_response(),
        };
        let reply = json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
        let (content_type, body) = if state.sse {
            (
                "text/event-stream",
                format!("event: message\ndata: {reply}\n\n"),
            )
        } else {
            ("application/json", reply.to_string())
        };
        (
            [
                ("content-type", content_type),
                ("mcp-session-id", "test-session"),
            ],
            body,
        )
            .into_response()
    }

    impl Fixture {
        async fn start(sse: bool) -> Self {
            let state = FixtureState {
                sse,
                ..Default::default()
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
            let router = Router::new()
                .route("/mcp", any(handler))
                .with_state(state.clone());
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            Self {
                endpoint,
                state,
                server,
            }
        }

        fn registry(&self, timeout: Duration) -> IntegrationRegistry {
            let mut browser = BrowserIntegration::new();
            browser.parallel = ParallelSearch {
                endpoint: self.endpoint.clone(),
                timeout,
            };
            let mut registry = IntegrationRegistry::new();
            registry.register(Arc::new(browser)).unwrap();
            registry
        }
    }

    fn call(query: &str) -> ToolCall {
        ToolCall {
            id: "search-call".into(),
            name: "browser_search".into(),
            arguments: json!({"query": query, "provider": "parallel"}).to_string(),
        }
    }

    #[test]
    fn omitted_null_and_auto_provider_preserve_the_existing_chain() {
        for arguments in [
            json!({"query": "rust"}),
            json!({"query": "rust", "provider": null}),
            json!({"query": "rust", "provider": "auto"}),
        ] {
            let args: BrowserSearchArgs = serde_json::from_value(arguments).unwrap();
            assert!(matches!(
                args.provider.unwrap_or(BrowserSearchProvider::Auto),
                BrowserSearchProvider::Auto
            ));
        }
        assert!(
            serde_json::from_value::<BrowserSearchArgs>(
                json!({"query": "rust", "provider": "unknown"})
            )
            .is_err()
        );
        assert_eq!(
            ParallelSearch::default().endpoint,
            "https://search.parallel.ai/mcp"
        );
    }

    #[tokio::test]
    async fn dispatcher_returns_sources_with_json_and_sse_transport() {
        for sse in [false, true] {
            let fixture = Fixture::start(sse).await;
            let registry = fixture.registry(Duration::from_secs(5));
            // The same Web capability controls discovery for both providers.
            assert!(
                !registry
                    .agent_tool_definitions_for(&Default::default())
                    .iter()
                    .any(|t| t.name == "browser_search")
            );
            assert!(
                registry
                    .agent_tool_definitions_for(&[CapabilityGroup::Web].into())
                    .iter()
                    .any(|t| t.name == "browser_search")
            );
            let result = registry.execute(&call("rust language")).await;
            assert!(!result.is_error, "{}", result.content);
            assert!(result.content.contains("Provider: Parallel"));
            // This is the tool-result message consumed by the existing agent loop.
            let message = Message::new(uuid::Uuid::new_v4(), Role::Tool, result.content)
                .with_tool_call_id("search-call".into());
            assert!(message.content.contains("https://www.rust-lang.org/"));
            assert!(message.content.contains("The Rust language home page."));
            let requests = fixture.state.requests.lock().unwrap();
            for request in requests.iter() {
                let uri = &request.uri;
                let headers = &request.headers;
                assert_eq!(uri.path(), "/mcp");
                assert_eq!(
                    headers["host"],
                    fixture
                        .endpoint
                        .strip_prefix("http://")
                        .unwrap()
                        .strip_suffix("/mcp")
                        .unwrap()
                );
                assert_eq!(headers["user-agent"], USER_AGENT);
                assert!(!headers.contains_key("authorization"));
            }
            let methods: Vec<_> = requests
                .iter()
                .filter_map(|r| r.body["method"].as_str())
                .collect();
            assert!(methods.contains(&"initialize"));
            assert!(methods.contains(&"tools/list"));
            let request = &requests
                .iter()
                .find(|r| r.body["method"] == "tools/call")
                .unwrap()
                .body;
            assert_eq!(request["params"]["name"], "web_search");
            assert_eq!(
                request["params"]["arguments"],
                json!({"objective": "rust language", "search_queries": ["rust language"]})
            );
        }
    }

    #[tokio::test]
    async fn dispatcher_surfaces_server_errors_and_empty_results_without_fallback() {
        let fixture = Fixture::start(false).await;
        let registry = fixture.registry(Duration::from_secs(5));
        for (query, expected) in [
            ("error", "quota exhausted"),
            ("empty", "returned no text"),
            (" ", "must not be empty"),
        ] {
            let result = registry.execute(&call(query)).await;
            assert!(result.is_error);
            assert!(result.content.contains(expected), "{}", result.content);
        }
        // Only explicitly selected Parallel calls occurred; no legacy search attempt.
        assert_eq!(
            fixture
                .state
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.body["method"] == "tools/call")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn dispatcher_reports_deadline_expiry() {
        let fixture = Fixture::start(false).await;
        let registry = fixture.registry(Duration::from_millis(100));
        let result = registry.execute(&call("timeout")).await;
        assert!(result.is_error);
        assert!(result.content.contains("timed out"), "{}", result.content);
        // The deadline also covers initialization, so it may expire before a
        // session exists. Established-session cleanup is verified below after
        // explicitly waiting for the tool call to reach the server.
    }

    #[tokio::test]
    async fn cancelling_dispatch_closes_the_mcp_session() {
        let fixture = Fixture::start(false).await;
        let registry = fixture.registry(Duration::from_secs(10));
        let task = tokio::spawn(async move { registry.execute(&call("timeout")).await });
        tokio::time::timeout(Duration::from_secs(10), fixture.state.called.notified())
            .await
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(10), fixture.state.closed.notified())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "calls the public anonymous Parallel MCP service"]
    async fn live_anonymous_search_through_dispatcher() {
        let mut registry = IntegrationRegistry::new();
        registry
            .register(Arc::new(BrowserIntegration::new()))
            .unwrap();
        let result = registry
            .execute(&call("Rust programming language official documentation"))
            .await;
        println!("{}", result.content);
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("https://"));
        assert!(result.content.contains("rust"));
    }
}
