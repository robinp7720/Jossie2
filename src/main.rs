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

    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    if std::env::var("JOSSIE_LOG_JSON").is_ok() {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    let config_str = std::fs::read_to_string("config.toml")
        .map_err(|e| anyhow::anyhow!("Failed to read config.toml: {e}"))?;
    let mut config: AppConfig = toml::from_str(&config_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse config.toml: {e}"))?;

    // Override secrets from environment variables
    override_config_from_env(&mut config);
    validate_llm_config(&mut config);

    tracing::info!("Connecting to database...");
    let db = Database::new(&config.database.url).await?;
    db.migrate().await?;
    let db = Arc::new(db);

    let mut llm = LlmClient::new(&config.llm.api_url, &config.llm.api_key, &config.llm.model);
    llm.set_reasoning_effort(config.llm.reasoning_effort.clone());
    llm.set_enable_web_search(config.llm.enable_web_search);
    llm.set_service_tier(config.llm.service_tier.clone());

    // Initialize KG LLM client - use cheaper model if configured, otherwise use primary model
    let kg_llm = if let Some(kg_model) = &config.llm.kg_model {
        tracing::info!(
            "Using dedicated model for knowledge graph extraction: {}",
            kg_model
        );
        let mut client = LlmClient::new(&config.llm.api_url, &config.llm.api_key, kg_model);
        client.set_reasoning_effort(config.llm.reasoning_effort.clone());
        client.set_service_tier(config.llm.service_tier.clone());
        client
    } else {
        tracing::info!("Using primary model for knowledge graph extraction");
        llm.clone()
    };

    let mut registry = IntegrationRegistry::new();
    registry.register(Arc::new(MemoryIntegration::new(db.clone())));
    registry.register(Arc::new(jossie_integration_files::FilesIntegration::new(
        db.clone(),
    )));

    // Knowledge Graph
    registry.register(Arc::new(jossie_integration_graph::GraphIntegration::new(
        db.clone(),
    )));

    let mut email = jossie_integration_email::EmailIntegration::new(&config.email);
    email.set_db(db.clone());
    let email = Arc::new(email);
    registry.register(email.clone());
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

    registry.register(Arc::new(jossie_integration_mail::MailIntegration::new(
        email.clone(),
        google_integration.clone(),
    )));
    tracing::info!("Registered mail integration");

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

    let (event_tx, _) = tokio::sync::broadcast::channel(512);
    let max_agent_iterations = config.llm.max_agent_iterations.min(64);
    if max_agent_iterations != config.llm.max_agent_iterations {
        tracing::warn!(
            "Configured llm.max_agent_iterations={} exceeds the hard cap of 64; clamping",
            config.llm.max_agent_iterations
        );
    }

    let state = Arc::new(AppState {
        db: db.clone(),
        llm,
        kg_llm,
        registry: Arc::new(registry),
        auth_token: config.server.auth_token.clone(),
        auth_password_hash: config.server.auth_password_hash.clone(),
        session_cookie_secure: config.server.session_cookie_secure.unwrap_or_else(|| {
            config
                .server
                .public_base_url
                .as_deref()
                .is_some_and(|url| url.starts_with("https://"))
        }),
        public_base_url: config.server.public_base_url.clone(),
        system_prompt: config.llm.system_prompt.clone(),
        max_agent_iterations,
        max_context_messages: config.llm.max_context_messages,
        event_max_context_messages: config.llm.event_max_context_messages,
        google_config: config.google.clone(),
        google_integration,
        telegram_token: config.telegram.bot_token.clone(),
        enable_self_reflection: config.llm.enable_self_reflection,
        active_conversations: Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
        cancelled_conversations: Arc::new(tokio::sync::RwLock::new(
            std::collections::HashSet::new(),
        )),
        pending_google_oauth: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        event_tx,
        cors_origins: config.server.cors_origins.clone(),
        max_request_body_bytes: config.server.max_request_body_bytes,
    });

    // Start Telegram bot if configured
    if !config.telegram.bot_token.is_empty() {
        let bot = jossie_telegram::TelegramBot::new(
            &config.telegram.bot_token,
            state.clone(),
            config.telegram.allowed_user_id,
        );
        tokio::spawn(async move {
            if let Err(e) = bot.run().await {
                tracing::error!("Telegram bot error: {e}");
            }
        });
    }

    // Start the event loop even without Telegram so scheduled tasks and web-visible
    // background activity continue to run.
    let event_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = event_loop::start_event_loop(event_state).await {
            tracing::error!("Event loop error: {e}");
        }
    });

    let app = jossie_server::router(state);
    let addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!("Starting server on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn validate_llm_config(config: &mut AppConfig) {
    const MIN_CHAT_CONTEXT_MESSAGES: usize = 20;
    const MIN_EVENT_CONTEXT_MESSAGES: usize = 8;

    if config.llm.max_context_messages < MIN_CHAT_CONTEXT_MESSAGES {
        tracing::warn!(
            "llm.max_context_messages={} is too low; clamping to {}",
            config.llm.max_context_messages,
            MIN_CHAT_CONTEXT_MESSAGES
        );
        config.llm.max_context_messages = MIN_CHAT_CONTEXT_MESSAGES;
    }

    if config.llm.event_max_context_messages < MIN_EVENT_CONTEXT_MESSAGES {
        tracing::warn!(
            "llm.event_max_context_messages={} is too low; clamping to {}",
            config.llm.event_max_context_messages,
            MIN_EVENT_CONTEXT_MESSAGES
        );
        config.llm.event_max_context_messages = MIN_EVENT_CONTEXT_MESSAGES;
    }

    if let Some(service_tier) = &mut config.llm.service_tier {
        let normalized = service_tier.trim().to_string();
        if normalized.is_empty() {
            config.llm.service_tier = None;
        } else if normalized != *service_tier {
            *service_tier = normalized;
        }
    }
}

fn override_config_from_env(config: &mut AppConfig) {
    use std::env;

    if let Ok(val) = env::var("JOSSIE_SERVER_AUTH_TOKEN") {
        config.server.auth_token = val;
    }
    if let Ok(val) = env::var("JOSSIE_SERVER_AUTH_PASSWORD_HASH") {
        config.server.auth_password_hash = val;
    }
    if let Ok(val) = env::var("JOSSIE_SERVER_PUBLIC_BASE_URL") {
        let trimmed = val.trim();
        config.server.public_base_url = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Ok(val) = env::var("JOSSIE_LLM_API_KEY") {
        config.llm.api_key = val;
    }
    if let Ok(val) = env::var("JOSSIE_LLM_SYSTEM_PROMPT") {
        config.llm.system_prompt = val;
    }
    if let Ok(val) = env::var("JOSSIE_LLM_ENABLE_WEB_SEARCH") {
        if let Some(parsed) = parse_env_bool(&val) {
            config.llm.enable_web_search = parsed;
        }
    }
    if let Ok(val) = env::var("JOSSIE_LLM_SERVICE_TIER") {
        config.llm.service_tier = Some(val);
    }
    if let Ok(val) = env::var("JOSSIE_LLM_MAX_CONTEXT_MESSAGES") {
        if let Ok(parsed) = val.parse::<usize>() {
            config.llm.max_context_messages = parsed;
        }
    }
    if let Ok(val) = env::var("JOSSIE_LLM_EVENT_MAX_CONTEXT_MESSAGES") {
        if let Ok(parsed) = val.parse::<usize>() {
            config.llm.event_max_context_messages = parsed;
        }
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

fn parse_env_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
