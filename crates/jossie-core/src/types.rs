use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system" => Ok(Role::System),
            "user" => Ok(Role::User),
            "assistant" => Ok(Role::Assistant),
            "tool" => Ok(Role::Tool),
            other => Err(format!("unknown role: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Uuid,
    pub name: String,
    pub mime_type: Option<String>,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    /// Exact Responses API output items needed for an in-flight continuation.
    /// These may include hidden reasoning and must never be exposed or persisted.
    #[serde(skip)]
    pub response_items: Option<Vec<serde_json::Value>>,
    pub created_at: DateTime<Utc>,
}

impl Message {
    /// Create a new message with default optional fields.
    pub fn new(conversation_id: Uuid, role: Role, content: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            conversation_id,
            role,
            content,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            attachments: None,
            response_items: None,
            created_at: Utc::now(),
        }
    }

    /// Create a transient message (nil IDs) for prompt construction.
    pub fn transient(role: Role, content: String) -> Self {
        Self {
            id: Uuid::nil(),
            conversation_id: Uuid::nil(),
            role,
            content,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            attachments: None,
            response_items: None,
            created_at: Utc::now(),
        }
    }

    /// Set the tool_calls field.
    pub fn with_tool_calls(mut self, tool_calls: serde_json::Value) -> Self {
        self.tool_calls = Some(tool_calls);
        self
    }

    /// Set the tool_call_id field.
    pub fn with_tool_call_id(mut self, tool_call_id: String) -> Self {
        self.tool_call_id = Some(tool_call_id);
        self
    }

    /// Set the name field.
    pub fn with_name(mut self, name: String) -> Self {
        self.name = Some(name);
        self
    }

    /// Set the attachments field.
    pub fn with_attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.attachments = Some(attachments);
        self
    }

    /// Preserve Responses API output items for the next call in the same run.
    pub fn with_response_items(mut self, response_items: Vec<serde_json::Value>) -> Self {
        if !response_items.is_empty() {
            self.response_items = Some(response_items);
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Uuid,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_roundtrip() {
        for role in [Role::System, Role::User, Role::Assistant, Role::Tool] {
            let s = role.as_str();
            let parsed: Role = s.parse().unwrap();
            assert_eq!(parsed, role);
        }
    }

    #[test]
    fn role_serde_roundtrip() {
        for role in [Role::System, Role::User, Role::Assistant, Role::Tool] {
            let json = serde_json::to_string(&role).unwrap();
            let parsed: Role = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, role);
        }
    }

    #[test]
    fn role_display() {
        assert_eq!(Role::System.to_string(), "system");
        assert_eq!(Role::User.to_string(), "user");
        assert_eq!(Role::Assistant.to_string(), "assistant");
        assert_eq!(Role::Tool.to_string(), "tool");
    }

    #[test]
    fn role_from_str_invalid() {
        assert!("invalid".parse::<Role>().is_err());
    }

    #[test]
    fn response_items_are_not_publicly_serialized() {
        let message = Message::transient(Role::Assistant, "Working".to_string())
            .with_response_items(vec![serde_json::json!({
                "type": "reasoning",
                "id": "rs_private"
            })]);

        let json = serde_json::to_value(message).unwrap();
        assert!(json.get("response_items").is_none());
    }
}
