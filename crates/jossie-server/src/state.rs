use std::sync::Arc;
use jossie_core::integration::IntegrationRegistry;
use jossie_db::Database;
use jossie_llm::LlmClient;

pub struct AppState {
    pub db: Arc<Database>,
    pub llm: LlmClient,
    pub registry: IntegrationRegistry,
    pub auth_token: String,
    pub system_prompt: String,
    pub max_agent_iterations: usize,
    pub google_config: jossie_core::config::GoogleConfig,
}
