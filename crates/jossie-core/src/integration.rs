use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// --- Tool Result Validation (#2) ---

#[derive(Debug, Clone, PartialEq)]
pub enum ResultQuality {
    Good,
    Empty,
    Partial,
    PossibleError,
}

pub fn validate_tool_result(tool_name: &str, content: &str) -> (ResultQuality, Option<String>) {
    let trimmed = content.trim();

    // Check for empty results
    if trimmed.is_empty()
        || trimmed == "[]"
        || trimmed == "{}"
        || trimmed == "null"
        || trimmed == "\"\""
    {
        return (
            ResultQuality::Empty,
            Some(format!(
                "[HINT: {tool_name} returned empty results. Consider trying different search terms or parameters.]"
            )),
        );
    }

    // Check for HTTP error patterns
    if trimmed.contains("403 Forbidden")
        || trimmed.contains("401 Unauthorized")
        || trimmed.contains("404 Not Found")
    {
        return (
            ResultQuality::PossibleError,
            Some(format!(
                "[HINT: {tool_name} returned an HTTP error. The resource may be inaccessible or the URL may be wrong.]"
            )),
        );
    }
    if trimmed.contains("500 Internal Server Error") || trimmed.contains("503 Service Unavailable")
    {
        return (
            ResultQuality::PossibleError,
            Some(format!(
                "[HINT: {tool_name} hit a server error. This may be transient - consider retrying later.]"
            )),
        );
    }

    // Check for truncation markers
    if trimmed.contains("[NOTICE: Output truncated") {
        return (
            ResultQuality::Partial,
            Some(format!(
                "[HINT: {tool_name} output was truncated. If the information you need is missing, consider a more narrow query.]"
            )),
        );
    }

    // Check for common error prefixes
    if trimmed.starts_with("Error:") || trimmed.starts_with("error:") {
        return (
            ResultQuality::PossibleError,
            Some(format!(
                "[HINT: {tool_name} returned an error. Review the error message and adjust your approach.]"
            )),
        );
    }

    (ResultQuality::Good, None)
}

// --- Error Recovery (#3) ---

#[derive(Debug, Clone, PartialEq)]
pub enum ToolErrorKind {
    Transient,
    BadInput,
    NotFound,
    AuthFailure,
    Unknown,
}

pub fn classify_error(error_msg: &str) -> ToolErrorKind {
    let lower = error_msg.to_lowercase();

    // Transient errors - safe to retry
    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("rate limit")
        || lower.contains("429")
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("temporarily unavailable")
    {
        return ToolErrorKind::Transient;
    }

    // Auth failures
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("authentication")
        || lower.contains("token expired")
    {
        return ToolErrorKind::AuthFailure;
    }

    // Not found
    if lower.contains("404") || lower.contains("not found") || lower.contains("no such") {
        return ToolErrorKind::NotFound;
    }

    // Bad input
    if lower.contains("invalid")
        || lower.contains("bad request")
        || lower.contains("400")
        || lower.contains("missing required")
        || lower.contains("malformed")
        || lower.contains("parse error")
    {
        return ToolErrorKind::BadInput;
    }

    ToolErrorKind::Unknown
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnboardingField {
    pub name: String,
    pub label: String,
    pub input_type: String,    // "text", "password", "oauth", "info"
    pub value: Option<String>, // Current value or auth URL
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "details")]
pub enum OnboardingStatus {
    Configured,
    RequiresAction { fields: Vec<OnboardingField> },
}

#[async_trait::async_trait]
pub trait Integration: Send + Sync {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<ToolDefinition>;
    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String>;
    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        Ok(OnboardingStatus::Configured)
    }
    async fn poll(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

pub struct IntegrationRegistry {
    integrations: Vec<Arc<dyn Integration>>,
    tool_map: HashMap<String, usize>,
}

impl IntegrationRegistry {
    pub fn new() -> Self {
        Self {
            integrations: Vec::new(),
            tool_map: HashMap::new(),
        }
    }

    pub fn register(&mut self, integration: Arc<dyn Integration>) {
        let idx = self.integrations.len();
        for tool in integration.tools() {
            self.tool_map.insert(tool.name.clone(), idx);
        }
        self.integrations.push(integration);
    }

    pub fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.integrations.iter().flat_map(|i| i.tools()).collect()
    }

    pub fn get_integrations(&self) -> &[Arc<dyn Integration>] {
        &self.integrations
    }

    pub async fn execute(&self, call: &ToolCall) -> ToolResult {
        let Some(&idx) = self.tool_map.get(&call.name) else {
            return ToolResult {
                tool_call_id: call.id.clone(),
                content: format!("Unknown tool: {}", call.name),
                is_error: true,
            };
        };

        const MAX_RETRIES: usize = 2;
        const MAX_OUTPUT_SIZE: usize = 100_000;
        let backoff_ms = [500, 1000];

        let mut last_error = String::new();

        for attempt in 0..=MAX_RETRIES {
            match self.integrations[idx]
                .execute(&call.name, &call.arguments)
                .await
            {
                Ok(content) => {
                    let original_len = content.len();
                    let mut final_content = content;

                    if original_len > MAX_OUTPUT_SIZE {
                        tracing::warn!(
                            "Tool '{}' returned large output: {} chars. Truncating to {} chars.",
                            call.name,
                            original_len,
                            MAX_OUTPUT_SIZE
                        );
                        final_content.truncate(MAX_OUTPUT_SIZE);
                        final_content.push_str(&format!(
                            "\n\n[NOTICE: Output truncated to {} characters for efficiency. Original size: {} chars. If essential information is missing, please try a more specific query.]",
                            MAX_OUTPUT_SIZE,
                            original_len
                        ));
                    }

                    // Validate the result and append hints
                    let (quality, hint) = validate_tool_result(&call.name, &final_content);
                    if let Some(hint_text) = hint {
                        tracing::debug!("Tool '{}' result quality: {:?}", call.name, quality);
                        final_content.push('\n');
                        final_content.push_str(&hint_text);
                    }

                    return ToolResult {
                        tool_call_id: call.id.clone(),
                        content: final_content,
                        is_error: false,
                    };
                }
                Err(e) => {
                    last_error = format!("{e}");
                    let error_kind = classify_error(&last_error);

                    match error_kind {
                        ToolErrorKind::Transient if attempt < MAX_RETRIES => {
                            let delay = backoff_ms[attempt];
                            tracing::warn!(
                                "Tool '{}' transient error (attempt {}/{}): {}. Retrying in {}ms...",
                                call.name,
                                attempt + 1,
                                MAX_RETRIES + 1,
                                last_error,
                                delay
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(delay as u64))
                                .await;
                            continue;
                        }
                        ToolErrorKind::BadInput => {
                            return ToolResult {
                                tool_call_id: call.id.clone(),
                                content: format!(
                                    "Error: {last_error}\n[HINT: Bad input - do not retry with the same arguments. Check parameter types and required fields.]"
                                ),
                                is_error: true,
                            };
                        }
                        ToolErrorKind::AuthFailure => {
                            return ToolResult {
                                tool_call_id: call.id.clone(),
                                content: format!(
                                    "Error: {last_error}\n[HINT: Authentication failure. The integration credentials may need to be refreshed.]"
                                ),
                                is_error: true,
                            };
                        }
                        ToolErrorKind::NotFound => {
                            return ToolResult {
                                tool_call_id: call.id.clone(),
                                content: format!(
                                    "Error: {last_error}\n[HINT: Resource not found. Verify the identifier or path is correct.]"
                                ),
                                is_error: true,
                            };
                        }
                        _ => {
                            // Transient that exhausted retries, or Unknown
                            break;
                        }
                    }
                }
            }
        }

        ToolResult {
            tool_call_id: call.id.clone(),
            content: format!("Error: {last_error}"),
            is_error: true,
        }
    }
}

impl Default for IntegrationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct MockIntegration;

    #[async_trait::async_trait]
    impl Integration for MockIntegration {
        fn name(&self) -> &str {
            "mock"
        }
        fn tools(&self) -> Vec<ToolDefinition> {
            vec![
                ToolDefinition {
                    name: "mock_tool".to_string(),
                    description: "A mock tool".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {},
                        "required": [],
                        "additionalProperties": false
                    }),
                },
                ToolDefinition {
                    name: "mock_echo".to_string(),
                    description: "Echoes input".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                        "additionalProperties": false
                    }),
                },
            ]
        }
        async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
            match tool_name {
                "mock_tool" => Ok("mock result".to_string()),
                "mock_echo" => Ok(format!("echo: {arguments}")),
                _ => anyhow::bail!("unknown tool"),
            }
        }
    }

    #[test]
    fn registry_collects_tool_definitions() {
        let mut reg = IntegrationRegistry::new();
        assert_eq!(reg.all_tool_definitions().len(), 0);
        reg.register(Arc::new(MockIntegration));
        let tools = reg.all_tool_definitions();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "mock_tool");
        assert_eq!(tools[1].name, "mock_echo");
    }

    #[tokio::test]
    async fn registry_dispatches_to_correct_integration() {
        let mut reg = IntegrationRegistry::new();
        reg.register(Arc::new(MockIntegration));

        let call = ToolCall {
            id: "call_1".to_string(),
            name: "mock_tool".to_string(),
            arguments: "{}".to_string(),
        };
        let result = reg.execute(&call).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "mock result");
    }

    #[tokio::test]
    async fn registry_returns_error_for_unknown_tool() {
        let reg = IntegrationRegistry::new();
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "nonexistent".to_string(),
            arguments: "{}".to_string(),
        };
        let result = reg.execute(&call).await;
        assert!(result.is_error);
        assert!(result.content.contains("Unknown tool"));
    }

    #[tokio::test]
    async fn registry_execute_echo() {
        let mut reg = IntegrationRegistry::new();
        reg.register(Arc::new(MockIntegration));

        let call = ToolCall {
            id: "call_2".to_string(),
            name: "mock_echo".to_string(),
            arguments: r#"{"text":"hello"}"#.to_string(),
        };
        let result = reg.execute(&call).await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[test]
    fn test_validate_empty_results() {
        let (q, hint) = validate_tool_result("memory_search", "[]");
        assert_eq!(q, ResultQuality::Empty);
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("empty results"));

        let (q, _) = validate_tool_result("test", "{}");
        assert_eq!(q, ResultQuality::Empty);

        let (q, _) = validate_tool_result("test", "");
        assert_eq!(q, ResultQuality::Empty);
    }

    #[test]
    fn test_validate_good_results() {
        let (q, hint) = validate_tool_result("test", "some valid content here");
        assert_eq!(q, ResultQuality::Good);
        assert!(hint.is_none());
    }

    #[test]
    fn test_validate_http_errors() {
        let (q, _) = validate_tool_result("http_get", "403 Forbidden");
        assert_eq!(q, ResultQuality::PossibleError);

        let (q, _) = validate_tool_result("http_get", "404 Not Found");
        assert_eq!(q, ResultQuality::PossibleError);
    }

    #[test]
    fn test_validate_truncated() {
        let (q, _) = validate_tool_result(
            "test",
            "data...\n[NOTICE: Output truncated to 100000 characters for efficiency. Original size: 200000 chars. If essential information is missing, please try a more specific query.]",
        );
        assert_eq!(q, ResultQuality::Partial);
    }

    #[test]
    fn test_classify_transient_errors() {
        assert_eq!(
            classify_error("connection timeout"),
            ToolErrorKind::Transient
        );
        assert_eq!(
            classify_error("rate limit exceeded"),
            ToolErrorKind::Transient
        );
        assert_eq!(classify_error("HTTP 503"), ToolErrorKind::Transient);
        assert_eq!(
            classify_error("connection refused"),
            ToolErrorKind::Transient
        );
    }

    #[test]
    fn test_classify_bad_input() {
        assert_eq!(
            classify_error("invalid parameter 'foo'"),
            ToolErrorKind::BadInput
        );
        assert_eq!(classify_error("400 Bad Request"), ToolErrorKind::BadInput);
        assert_eq!(
            classify_error("missing required field"),
            ToolErrorKind::BadInput
        );
    }

    #[test]
    fn test_classify_auth_errors() {
        assert_eq!(
            classify_error("401 Unauthorized"),
            ToolErrorKind::AuthFailure
        );
        assert_eq!(classify_error("403 Forbidden"), ToolErrorKind::AuthFailure);
        assert_eq!(classify_error("token expired"), ToolErrorKind::AuthFailure);
    }

    #[test]
    fn test_classify_not_found() {
        assert_eq!(classify_error("404 not found"), ToolErrorKind::NotFound);
        assert_eq!(
            classify_error("no such file or directory"),
            ToolErrorKind::NotFound
        );
    }

    #[test]
    fn test_classify_unknown() {
        assert_eq!(
            classify_error("something weird happened"),
            ToolErrorKind::Unknown
        );
    }

    #[tokio::test]
    async fn test_tool_output_truncation() {
        let mut reg = IntegrationRegistry::new();

        struct LargeIntegration;
        #[async_trait::async_trait]
        impl Integration for LargeIntegration {
            fn name(&self) -> &str {
                "large"
            }
            fn tools(&self) -> Vec<ToolDefinition> {
                vec![ToolDefinition {
                    name: "large_tool".to_string(),
                    description: "desc".to_string(),
                    parameters: serde_json::json!({}),
                }]
            }
            async fn execute(&self, _name: &str, _args: &str) -> anyhow::Result<String> {
                Ok("a".repeat(150_000))
            }
        }

        reg.register(Arc::new(LargeIntegration));
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "large_tool".to_string(),
            arguments: "{}".to_string(),
        };

        let result = reg.execute(&call).await;
        assert!(result.content.contains("Original size: 150000 chars"));
        // Content includes truncation marker + validation hint for partial results
        assert!(result.content.contains("[NOTICE: Output truncated"));
        assert!(result.content.contains("[HINT:"));
    }
}
