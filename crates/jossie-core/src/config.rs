use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub llm: LlmConfig,
    pub database: DatabaseConfig,
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub email: EmailConfig,
    #[serde(default)]
    pub google: GoogleConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub auth_token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
    #[serde(default = "default_max_iterations")]
    pub max_agent_iterations: usize,
}

fn default_system_prompt() -> String {
    "You are Jossie, a helpful assistant. Use the available tools when needed to help the user.".to_string()
}

fn default_max_iterations() -> usize {
    20
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub bot_token: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmailConfig {
    #[serde(default)]
    pub imap_host: String,
    #[serde(default)]
    pub imap_port: u16,
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default)]
    pub smtp_port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GoogleConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub refresh_token: String,
}
