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
    reg.register(Arc::new(MockIntegration)).unwrap();
    let tools = reg.all_tool_definitions();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "mock_tool");
    assert_eq!(tools[1].name, "mock_echo");
}

#[test]
fn registry_rejects_duplicate_integrations_without_mutating_definitions() {
    let mut registry = IntegrationRegistry::new();
    registry.register(Arc::new(MockIntegration)).unwrap();

    let error = registry.register(Arc::new(MockIntegration)).unwrap_err();

    assert!(error.to_string().contains("already registered"));
    assert_eq!(registry.all_tool_definitions().len(), 2);
}

#[test]
fn built_in_tool_metadata_separates_reads_from_actions() {
    let search = tool_metadata("mail_search", r#"{"query":"status"}"#);
    assert_eq!(search.capability, CapabilityGroup::Mail);
    assert_eq!(search.effect, ToolEffect::Read);
    assert!(search.concurrent);
    assert!(search.retry_transient);

    let send = tool_metadata("mail_send", r#"{"to":"owner@example.com"}"#);
    assert_eq!(send.capability, CapabilityGroup::Mail);
    assert_eq!(send.effect, ToolEffect::ExternalWrite);
    assert!(!send.concurrent);
    assert!(!send.retry_transient);

    let delete = tool_metadata("memory_delete", r#"{"key":"old"}"#);
    assert_eq!(delete.effect, ToolEffect::LocalWrite);
    assert!(!delete.requires_approval);
}

#[test]
fn personal_tool_metadata_has_expected_approval_policy() {
    let task_read = tool_metadata("task_list", "{}");
    assert_eq!(task_read.capability, CapabilityGroup::Tasks);
    assert_eq!(task_read.effect, ToolEffect::Read);

    for (tool, capability) in [
        ("task_complete", CapabilityGroup::Tasks),
        ("notes_append", CapabilityGroup::Notes),
        ("media_play", CapabilityGroup::Media),
    ] {
        let metadata = tool_metadata(tool, "{}");
        assert_eq!(metadata.capability, capability);
        assert_eq!(metadata.effect, ToolEffect::ExternalWrite);
        assert!(metadata.requires_approval);
    }

    let home_action = tool_metadata("home_call_service", "{}");
    assert_eq!(home_action.capability, CapabilityGroup::Home);
    assert_eq!(home_action.effect, ToolEffect::ExternalWrite);
    assert!(!home_action.requires_approval);
}

#[test]
fn http_effect_depends_on_method() {
    assert_eq!(
        tool_metadata("http_request", r#"{"method":"GET"}"#).effect,
        ToolEffect::Read
    );
    assert_eq!(
        tool_metadata("http_request", r#"{"method":"POST"}"#).effect,
        ToolEffect::ExternalWrite
    );
    assert_eq!(
        tool_metadata("http_request", r#"{"method":"DELETE"}"#).effect,
        ToolEffect::Destructive
    );
}

#[tokio::test]
async fn registry_dispatches_to_correct_integration() {
    let mut reg = IntegrationRegistry::new();
    reg.register(Arc::new(MockIntegration)).unwrap();

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
    reg.register(Arc::new(MockIntegration)).unwrap();

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

#[test]
fn tool_schema_comes_from_argument_type() {
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Args {
        required_value: String,
        optional_value: Option<u32>,
    }

    let tool = ToolDefinition::for_args::<Args>("typed", "typed arguments");
    assert_eq!(tool.parameters["type"], "object");
    assert_eq!(tool.parameters["additionalProperties"], false);
    assert_eq!(
        tool.parameters["required"],
        serde_json::json!(["required_value"])
    );
    assert!(
        tool.parameters["properties"]
            .get("optional_value")
            .is_some()
    );
    assert!(tool.parameters.get("$schema").is_none());
}

#[tokio::test]
async fn test_tool_output_truncation() {
    let mut reg = IntegrationRegistry::with_max_output_chars(1_000);

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

    reg.register(Arc::new(LargeIntegration)).unwrap();
    let call = ToolCall {
        id: "call_1".to_string(),
        name: "large_tool".to_string(),
        arguments: "{}".to_string(),
    };

    let result = reg.execute(&call).await;
    assert!(result.content.contains("Original size: 150000 chars"));
    assert!(result.content.starts_with(&"a".repeat(1_000)));
    // Content includes truncation marker + validation hint for partial results
    assert!(result.content.contains("[NOTICE: Output truncated"));
    assert!(result.content.contains("[HINT:"));
}
