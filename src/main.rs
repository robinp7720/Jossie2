use std::sync::Arc;
use anyhow::Result;
use jossie_core::config::AppConfig;
use jossie_core::integration::IntegrationRegistry;
use jossie_db::Database;
use jossie_llm::LlmClient;
use jossie_integration_memory::MemoryIntegration;
use jossie_server::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config_str = std::fs::read_to_string("config.toml")
        .expect("Failed to read config.toml");
    let config: AppConfig = toml::from_str(&config_str)
        .expect("Failed to parse config.toml");

    tracing::info!("Connecting to database...");
    let db = Database::new(&config.database.url).await?;
    db.migrate().await?;
    let db = Arc::new(db);

    let llm = LlmClient::new(&config.llm.api_url, &config.llm.api_key, &config.llm.model);

    let mut registry = IntegrationRegistry::new();
    registry.register(Arc::new(MemoryIntegration::new(db.clone())));

    if !config.email.imap_host.is_empty() {
        registry.register(Arc::new(jossie_integration_email::EmailIntegration::new(&config.email)));
        tracing::info!("Registered email integration");
    }

    if !config.google.client_id.is_empty() {
        registry.register(Arc::new(jossie_integration_google::GoogleIntegration::new(&config.google)));
        tracing::info!("Registered Google integration");
    }

    tracing::info!("Registered {} tool(s)", registry.all_tool_definitions().len());

    let state = Arc::new(AppState {
        db,
        llm,
        registry,
        auth_token: config.server.auth_token.clone(),
        system_prompt: config.llm.system_prompt.clone(),
        max_agent_iterations: config.llm.max_agent_iterations,
    });

    // Start Telegram bot if configured
    if !config.telegram.bot_token.is_empty() {
        let bot = jossie_telegram::TelegramBot::new(&config.telegram.bot_token, state.clone());
        tokio::spawn(async move {
            if let Err(e) = bot.run().await {
                tracing::error!("Telegram bot error: {e}");
            }
        });
    }

    let app = jossie_server::router(state);
    let addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!("Starting server on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
