use jossie_core::integration::IntegrationRegistry;
use jossie_db::Database;
use jossie_integration_google::GoogleIntegration;
use jossie_llm::LlmClient;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct AppState {
    pub db: Arc<Database>,
    pub llm: LlmClient,
    pub kg_llm: LlmClient,
    pub registry: Arc<IntegrationRegistry>,
    pub auth_token: String,
    pub system_prompt: String,
    pub max_agent_iterations: usize,
    pub max_context_messages: usize,
    pub event_max_context_messages: usize,
    pub google_config: jossie_core::config::GoogleConfig,
    pub google_integration: Option<Arc<GoogleIntegration>>,
    pub telegram_token: String,
    pub enable_self_reflection: bool,
    pub active_conversations: Arc<RwLock<HashSet<Uuid>>>,
    pub cors_origins: Vec<String>,
    pub max_request_body_bytes: usize,
}
