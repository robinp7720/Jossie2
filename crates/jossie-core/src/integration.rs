use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityGroup {
    Core,
    Memory,
    Knowledge,
    Files,
    Mail,
    Calendar,
    Drive,
    Web,
    Scheduler,
}

impl CapabilityGroup {
    pub const ACTIVATABLE: [Self; 7] = [
        Self::Knowledge,
        Self::Files,
        Self::Mail,
        Self::Calendar,
        Self::Drive,
        Self::Web,
        Self::Scheduler,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Memory => "memory",
            Self::Knowledge => "knowledge",
            Self::Files => "files",
            Self::Mail => "mail",
            Self::Calendar => "calendar",
            Self::Drive => "drive",
            Self::Web => "web",
            Self::Scheduler => "scheduler",
        }
    }
}

impl std::str::FromStr for CapabilityGroup {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "core" => Ok(Self::Core),
            "memory" => Ok(Self::Memory),
            "knowledge" => Ok(Self::Knowledge),
            "files" => Ok(Self::Files),
            "mail" => Ok(Self::Mail),
            "calendar" => Ok(Self::Calendar),
            "drive" => Ok(Self::Drive),
            "web" => Ok(Self::Web),
            "scheduler" => Ok(Self::Scheduler),
            _ => Err(format!("Unknown capability group: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    Read,
    LocalWrite,
    ExternalWrite,
    Destructive,
}

impl ToolEffect {
    pub fn requires_explicit_authorization(self) -> bool {
        matches!(self, Self::ExternalWrite | Self::Destructive)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolMetadata {
    pub capability: CapabilityGroup,
    pub effect: ToolEffect,
    pub concurrent: bool,
    pub retry_transient: bool,
}

impl ToolMetadata {
    const fn read(capability: CapabilityGroup) -> Self {
        Self {
            capability,
            effect: ToolEffect::Read,
            concurrent: true,
            retry_transient: true,
        }
    }

    const fn local_write(capability: CapabilityGroup) -> Self {
        Self {
            capability,
            effect: ToolEffect::LocalWrite,
            concurrent: false,
            retry_transient: false,
        }
    }

    const fn action(capability: CapabilityGroup, effect: ToolEffect) -> Self {
        Self {
            capability,
            effect,
            concurrent: false,
            retry_transient: false,
        }
    }
}

/// Central policy for the current built-in tool set. Unknown tools fail safe as
/// serial external writes until their policy is added here.
pub fn tool_metadata(tool_name: &str, arguments: &str) -> ToolMetadata {
    let read = match tool_name {
        "memory_get"
        | "memory_generate_totp"
        | "memory_search"
        | "memory_list_keys"
        | "memory_list_all" => Some(CapabilityGroup::Memory),
        "graph_search" | "graph_list_by_type" | "graph_explore_connections" => {
            Some(CapabilityGroup::Knowledge)
        }
        "list_files" | "read_file" => Some(CapabilityGroup::Files),
        "mail_list_accounts"
        | "mail_search"
        | "mail_read"
        | "mail_list_mailboxes"
        | "email_list_accounts"
        | "email_search"
        | "email_read"
        | "email_list_folders"
        | "gmail_search"
        | "gmail_read"
        | "gmail_list_labels" => Some(CapabilityGroup::Mail),
        "google_list_accounts" | "calendar_list_calendars" | "calendar_list_events" => {
            Some(CapabilityGroup::Calendar)
        }
        "drive_search" | "drive_read" | "drive_list_files" => Some(CapabilityGroup::Drive),
        "browser_read_page"
        | "browser_session_snapshot"
        | "browser_navigate"
        | "browser_search" => Some(CapabilityGroup::Web),
        "list_scheduled_tasks" => Some(CapabilityGroup::Scheduler),
        _ => None,
    };
    if let Some(capability) = read {
        return ToolMetadata::read(capability);
    }

    match tool_name {
        "memory_save" => ToolMetadata::local_write(CapabilityGroup::Memory),
        "graph_upsert_node" | "graph_add_relation" => {
            ToolMetadata::local_write(CapabilityGroup::Knowledge)
        }
        "ingest_chat_export" => ToolMetadata::local_write(CapabilityGroup::Files),
        "browser_open_session" | "browser_close_session" => {
            ToolMetadata::local_write(CapabilityGroup::Web)
        }
        "memory_delete" => ToolMetadata::local_write(CapabilityGroup::Memory),
        "graph_delete_node" | "graph_delete_relation" => {
            ToolMetadata::local_write(CapabilityGroup::Knowledge)
        }
        "mail_send" | "email_send" | "gmail_send" => {
            ToolMetadata::action(CapabilityGroup::Mail, ToolEffect::ExternalWrite)
        }
        "calendar_create_event" | "calendar_update_event" => {
            ToolMetadata::action(CapabilityGroup::Calendar, ToolEffect::ExternalWrite)
        }
        "browser_fill_input" | "browser_click" | "browser_select_option" => {
            ToolMetadata::action(CapabilityGroup::Web, ToolEffect::ExternalWrite)
        }
        "schedule_task"
        | "schedule_recurring_task"
        | "schedule_cron_task"
        | "send_user_message" => {
            ToolMetadata::action(CapabilityGroup::Scheduler, ToolEffect::ExternalWrite)
        }
        "cancel_scheduled_task" => {
            ToolMetadata::action(CapabilityGroup::Scheduler, ToolEffect::Destructive)
        }
        "http_request" => {
            let method = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|value| value.get("method")?.as_str().map(str::to_uppercase))
                .unwrap_or_else(|| "GET".to_string());
            if matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS") {
                ToolMetadata::read(CapabilityGroup::Web)
            } else if method == "DELETE" {
                ToolMetadata::action(CapabilityGroup::Web, ToolEffect::Destructive)
            } else {
                ToolMetadata::action(CapabilityGroup::Web, ToolEffect::ExternalWrite)
            }
        }
        _ => ToolMetadata::action(CapabilityGroup::Core, ToolEffect::ExternalWrite),
    }
}

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
    fn agent_tools(&self) -> Vec<ToolDefinition> {
        self.tools()
    }
    fn show_in_onboarding(&self) -> bool {
        true
    }
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
    tool_definitions: Vec<ToolDefinition>,
    agent_tool_definitions: Vec<ToolDefinition>,
}

impl IntegrationRegistry {
    pub fn new() -> Self {
        Self {
            integrations: Vec::new(),
            tool_map: HashMap::new(),
            tool_definitions: Vec::new(),
            agent_tool_definitions: Vec::new(),
        }
    }

    pub fn register(&mut self, integration: Arc<dyn Integration>) {
        let idx = self.integrations.len();
        let tools = integration.tools();
        for tool in &tools {
            self.tool_map.insert(tool.name.clone(), idx);
        }
        self.tool_definitions.extend(tools);
        self.agent_tool_definitions
            .extend(integration.agent_tools());
        self.integrations.push(integration);
    }

    pub fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tool_definitions.clone()
    }

    pub fn all_agent_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.agent_tool_definitions.clone()
    }

    pub fn agent_tool_definitions_for(
        &self,
        capabilities: &std::collections::HashSet<CapabilityGroup>,
    ) -> Vec<ToolDefinition> {
        self.all_agent_tool_definitions()
            .into_iter()
            .filter(|tool| {
                capabilities.contains(&tool_metadata(&tool.name, "{}").capability)
                    || (tool.name == "google_list_accounts"
                        && capabilities.contains(&CapabilityGroup::Drive))
            })
            .collect()
    }

    pub fn has_agent_tools_for(&self, capability: CapabilityGroup) -> bool {
        self.all_agent_tool_definitions()
            .iter()
            .any(|tool| tool_metadata(&tool.name, "{}").capability == capability)
    }

    pub fn metadata_for(&self, call: &ToolCall) -> ToolMetadata {
        tool_metadata(&call.name, &call.arguments)
    }

    pub fn unclassified_agent_tools(&self) -> Vec<String> {
        self.all_agent_tool_definitions()
            .into_iter()
            .filter(|tool| tool_metadata(&tool.name, "{}").capability == CapabilityGroup::Core)
            .map(|tool| tool.name)
            .collect()
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
        const MAX_OUTPUT_SIZE: usize = 32_000;
        let backoff_ms = [500, 1000];

        let mut last_error = String::new();

        let retry_limit = if self.metadata_for(call).retry_transient {
            MAX_RETRIES
        } else {
            0
        };

        for attempt in 0..=retry_limit {
            match self.integrations[idx]
                .execute(&call.name, &call.arguments)
                .await
            {
                Ok(content) => {
                    let original_len = content.chars().count();
                    let mut final_content = content;

                    if original_len > MAX_OUTPUT_SIZE {
                        tracing::warn!(
                            "Tool '{}' returned large output: {} chars. Truncating to {} chars.",
                            call.name,
                            original_len,
                            MAX_OUTPUT_SIZE
                        );
                        final_content = final_content.chars().take(MAX_OUTPUT_SIZE).collect();
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
                        ToolErrorKind::Transient if attempt < retry_limit => {
                            let delay = backoff_ms.get(attempt).copied().unwrap_or(1000);
                            tracing::warn!(
                                "Tool '{}' transient error (attempt {}/{}): {}. Retrying in {}ms...",
                                call.name,
                                attempt + 1,
                                retry_limit + 1,
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
        assert!(!delete.effect.requires_explicit_authorization());
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
