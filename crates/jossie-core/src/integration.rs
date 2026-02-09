use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

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
        match self.integrations[idx]
            .execute(&call.name, &call.arguments)
            .await
        {
            Ok(content) => {
                let original_len = content.len();
                let mut final_content = content;
                const MAX_OUTPUT_SIZE: usize = 100_000;

                if original_len > MAX_OUTPUT_SIZE {
                    tracing::warn!(
                        "⚠️ Tool '{}' returned large output: {} chars. Truncating to {} chars.",
                        call.name,
                        original_len,
                        MAX_OUTPUT_SIZE
                    );
                    final_content.truncate(MAX_OUTPUT_SIZE);
                    final_content.push_str(&format!(
                        "\n... [Output truncated. Original size: {} chars]",
                        original_len
                    ));
                }

                ToolResult {
                    tool_call_id: call.id.clone(),
                    content: final_content,
                    is_error: false,
                }
            }
            Err(e) => ToolResult {
                tool_call_id: call.id.clone(),
                content: format!("Error: {e}"),
                is_error: true,
            },
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
        assert_eq!(
            result.content.len(),
            100_000 + "\n... [Output truncated. Original size: 150000 chars]".len()
        );
        assert!(result.content.contains("Original size: 150000 chars"));
    }
}
