use anyhow::Result;
use jossie_core::config::AppConfig;
use jossie_core::integration::IntegrationRegistry;
use jossie_db::Database;
use jossie_integration_http::HttpIntegration;
use jossie_integration_memory::MemoryIntegration;
use jossie_integration_scheduler::SchedulerIntegration;
use jossie_llm::LlmClient;
use jossie_server::AppState;
use std::sync::Arc;

mod event_loop;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if it exists
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config_str = std::fs::read_to_string("config.toml").expect("Failed to read config.toml");
    let mut config: AppConfig = toml::from_str(&config_str).expect("Failed to parse config.toml");

    // Override secrets from environment variables
    override_config_from_env(&mut config);

    tracing::info!("Connecting to database...");
    let db = Database::new(&config.database.url).await?;
    db.migrate().await?;
    let db = Arc::new(db);

    let llm = LlmClient::new(&config.llm.api_url, &config.llm.api_key, &config.llm.model);

    // Initialize KG LLM client - use cheaper model if configured, otherwise use primary model
    let kg_llm = if let Some(kg_model) = &config.llm.kg_model {
        tracing::info!(
            "Using dedicated model for knowledge graph extraction: {}",
            kg_model
        );
        LlmClient::new(&config.llm.api_url, &config.llm.api_key, kg_model)
    } else {
        tracing::info!("Using primary model for knowledge graph extraction");
        llm.clone()
    };

    let mut registry = IntegrationRegistry::new();
    registry.register(Arc::new(MemoryIntegration::new(db.clone())));

    // Knowledge Graph
    registry.register(Arc::new(jossie_integration_graph::GraphIntegration::new(
        db.clone(),
    )));

    let mut email = jossie_integration_email::EmailIntegration::new(&config.email);
    email.set_db(db.clone());
    registry.register(Arc::new(email));
    tracing::info!("Registered email integration");

    let mut google_integration: Option<Arc<jossie_integration_google::GoogleIntegration>> = None;
    if !config.google.client_id.is_empty() {
        let mut google = jossie_integration_google::GoogleIntegration::new(&config.google);
        google.set_db(db.clone());
        let google = Arc::new(google);
        registry.register(google.clone());
        google_integration = Some(google);
        tracing::info!("Registered Google integration");
    }

    // Browser Integration
    registry.register(Arc::new(
        jossie_integration_browser::BrowserIntegration::new(),
    ));
    tracing::info!("Registered browser integration");

    // HTTP Integration
    registry.register(Arc::new(HttpIntegration::new(
        config.http.allowed_domains.clone(),
    )));
    tracing::info!("Registered http integration");

    // Scheduler Integration
    registry.register(Arc::new(SchedulerIntegration::new(db.clone())));
    tracing::info!("Registered scheduler integration");

    tracing::info!(
        "Registered {} tool(s)",
        registry.all_tool_definitions().len()
    );

    let state = Arc::new(AppState {
        db,
        llm,
        kg_llm,
        registry,
        auth_token: config.server.auth_token.clone(),
        system_prompt: config.llm.system_prompt.clone(),
        max_agent_iterations: config.llm.max_agent_iterations,
        max_context_messages: config.llm.max_context_messages,
        google_config: config.google.clone(),
        google_integration,
        telegram_token: config.telegram.bot_token.clone(),
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

    // Start event loop if Telegram is configured
    // The event loop now handles integration events, scheduled tasks, and OOB messages
    if !state.telegram_token.is_empty() {
        let event_state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = event_loop::start_event_loop(event_state).await {
                tracing::error!("Event loop error: {e}");
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

fn override_config_from_env(config: &mut AppConfig) {
    use std::env;

    if let Ok(val) = env::var("JOSSIE_SERVER_AUTH_TOKEN") {
        config.server.auth_token = val;
    }
    if let Ok(val) = env::var("JOSSIE_LLM_API_KEY") {
        config.llm.api_key = val;
    }
    if let Ok(val) = env::var("JOSSIE_LLM_SYSTEM_PROMPT") {
        config.llm.system_prompt = val;
    }
    if let Ok(val) = env::var("JOSSIE_TELEGRAM_BOT_TOKEN") {
        config.telegram.bot_token = val.trim().to_string();
    }

    // Email
    if let Ok(val) = env::var("JOSSIE_EMAIL_USERNAME") {
        config.email.username = val;
    }
    if let Ok(val) = env::var("JOSSIE_EMAIL_PASSWORD") {
        config.email.password = val;
    }
    if let Ok(val) = env::var("JOSSIE_EMAIL_IMAP_HOST") {
        config.email.imap_host = val;
    }
    if let Ok(val) = env::var("JOSSIE_EMAIL_SMTP_HOST") {
        config.email.smtp_host = val;
    }

    // Google
    if let Ok(val) = env::var("JOSSIE_GOOGLE_CLIENT_ID") {
        config.google.client_id = val;
    }
    if let Ok(val) = env::var("JOSSIE_GOOGLE_CLIENT_SECRET") {
        config.google.client_secret = val;
    }
    if let Ok(val) = env::var("JOSSIE_GOOGLE_REFRESH_TOKEN") {
        config.google.refresh_token = val;
    }
}
