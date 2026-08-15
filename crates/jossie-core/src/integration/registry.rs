use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::{
    CapabilityGroup, ToolErrorKind, ToolMetadata, classify_error, tool_metadata,
    validate_tool_result,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    /// Build a tool definition from the same Rust type used to deserialize its arguments.
    pub fn for_args<A: schemars::JsonSchema>(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        let mut parameters = serde_json::to_value(schemars::schema_for!(A))
            .expect("JSON Schema serialization cannot fail");
        if let Some(object) = parameters.as_object_mut() {
            object.remove("$schema");
        }
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyToolArgs {}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct OnboardingField {
    pub name: String,
    pub label: String,
    pub input_type: String,    // "text", "password", "oauth", "info"
    pub value: Option<String>, // Current value or auth URL
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
