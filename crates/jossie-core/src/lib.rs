pub mod config;
pub mod error;
pub mod integration;
pub mod text;
pub mod types;

pub use config::AppConfig;
pub use error::JossieError;
pub use integration::{Integration, IntegrationRegistry, ToolCall, ToolDefinition, ToolResult};
pub use types::{Conversation, Message, Role};
