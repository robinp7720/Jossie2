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
    validate_telegram_config(&mut config);
    validate_heartbeat_config(&mut config);

    tracing::info!("Connecting to database...");
    let db = Database::new(&config.database.url).await?;
    db.migrate().await?;
    let interrupted_actions = db.mark_interrupted_actions_uncertain().await?;
    if interrupted_actions > 0 {
        tracing::warn!(
            "Marked {interrupted_actions} interrupted external actions as uncertain; they will not be retried"
        );
    }
    let interrupted_imports = db.requeue_interrupted_chat_imports().await?;
    if interrupted_imports > 0 {
        tracing::warn!("Requeued {interrupted_imports} interrupted chat import(s)");
    }
    let db = Arc::new(db);

    let mut llm = LlmClient::new(&config.llm.api_url, &config.llm.api_key, &config.llm.model);
    llm.set_reasoning_effort(config.llm.reasoning_effort.clone());
    llm.set_reasoning_context(config.llm.reasoning_context.clone());
    llm.set_enable_web_search(config.llm.enable_web_search);
    llm.set_service_tier(config.llm.service_tier.clone());
    llm.set_transcription_model(config.llm.transcription_model.clone());
    llm.set_max_attachment_bytes_per_request(config.llm.max_attachment_bytes_per_request);

    // Initialize KG LLM client - use cheaper model if configured, otherwise use primary model
    let kg_llm = if let Some(kg_model) = &config.llm.kg_model {
        tracing::info!(
            "Using dedicated model for knowledge graph extraction: {}",
            kg_model
        );
        let mut client = LlmClient::new(&config.llm.api_url, &config.llm.api_key, kg_model);
        client.set_reasoning_effort(config.llm.reasoning_effort.clone());
        client.set_reasoning_context(config.llm.reasoning_context.clone());
        client.set_service_tier(config.llm.service_tier.clone());
        client
    } else {
        tracing::info!("Using primary model for knowledge graph extraction");
        llm.clone()
    };

    let chat_export_importer = Arc::new(jossie_integration_files::ChatExportImporter::new(
        db.clone(),
        kg_llm.clone(),
        config.llm.openai_optimizations,
    ));
    let resumed_imports = chat_export_importer.resume_pending().await?;
    if resumed_imports > 0 {
        tracing::info!("Resumed {resumed_imports} queued chat import(s)");
    }

    let mut registry = IntegrationRegistry::new();
    registry.register(Arc::new(MemoryIntegration::new(db.clone())));
    registry.register(Arc::new(jossie_integration_files::FilesIntegration::new(
        db.clone(),
        chat_export_importer.clone(),
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

    let unclassified_tools = registry.unclassified_agent_tools();
    anyhow::ensure!(
        unclassified_tools.is_empty(),
        "Agent-visible tools are missing capability policy: {}",
        unclassified_tools.join(", ")
    );

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
        chat_export_importer,
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
        openai_optimizations: config.llm.openai_optimizations,
        max_context_chars: config.llm.max_context_chars,
        context_compact_target_chars: config.llm.context_compact_target_chars,
        context_keep_recent_dialogue_messages: config.llm.context_keep_recent_dialogue_messages,
        max_attachment_bytes_per_request: config.llm.max_attachment_bytes_per_request,
        google_config: config.google.clone(),
        google_integration,
        telegram_token: config.telegram.bot_token.clone(),
        telegram_max_download_bytes: config.telegram.max_download_bytes,
        telegram_ffmpeg_path: config.telegram.ffmpeg_path.clone(),
        enable_self_reflection: config.llm.enable_self_reflection,
        heartbeat_enabled: config.heartbeat.enabled,
        heartbeat_interval_secs: config.heartbeat.interval_seconds,
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
    const MIN_CONTEXT_CHARS: usize = 20_000;
    const MIN_RECENT_DIALOGUE_MESSAGES: usize = 4;
    const MIN_ATTACHMENT_BYTES: usize = 1024 * 1024;
    const MAX_ATTACHMENT_BYTES: usize = 50_000_000;

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

    if config.llm.max_context_chars < MIN_CONTEXT_CHARS {
        tracing::warn!(
            "llm.max_context_chars={} is too low; clamping to {}",
            config.llm.max_context_chars,
            MIN_CONTEXT_CHARS
        );
        config.llm.max_context_chars = MIN_CONTEXT_CHARS;
    }
    if config.llm.context_compact_target_chars >= config.llm.max_context_chars {
        config.llm.context_compact_target_chars = config.llm.max_context_chars * 2 / 3;
        tracing::warn!(
            "llm.context_compact_target_chars must be below max_context_chars; using {}",
            config.llm.context_compact_target_chars
        );
    }
    config.llm.context_keep_recent_dialogue_messages = config
        .llm
        .context_keep_recent_dialogue_messages
        .max(MIN_RECENT_DIALOGUE_MESSAGES);
    config.llm.max_attachment_bytes_per_request = config
        .llm
        .max_attachment_bytes_per_request
        .clamp(MIN_ATTACHMENT_BYTES, MAX_ATTACHMENT_BYTES);

    if let Some(model) = &mut config.llm.transcription_model {
        let normalized = model.trim().to_string();
        if normalized.is_empty() {
            config.llm.transcription_model = None;
        } else {
            *model = normalized;
        }
    }

    if let Some(service_tier) = &mut config.llm.service_tier {
        let normalized = service_tier.trim().to_string();
        if normalized.is_empty() {
            config.llm.service_tier = None;
        } else if normalized != *service_tier {
            *service_tier = normalized;
        }
    }

    if let Some(reasoning_context) = &mut config.llm.reasoning_context {
        let normalized = reasoning_context.trim().to_string();
        if normalized.is_empty() {
            config.llm.reasoning_context = None;
        } else if normalized != *reasoning_context {
            *reasoning_context = normalized;
        }
    }
}

fn validate_telegram_config(config: &mut AppConfig) {
    config.telegram.max_download_bytes = config
        .telegram
        .max_download_bytes
        .clamp(1, jossie_core::config::DEFAULT_TELEGRAM_MAX_DOWNLOAD_BYTES);
    config.telegram.ffmpeg_path = config.telegram.ffmpeg_path.trim().to_string();
    if config.telegram.ffmpeg_path.is_empty() {
        config.telegram.ffmpeg_path = "ffmpeg".to_string();
    }
}

fn validate_heartbeat_config(config: &mut AppConfig) {
    const MIN_HEARTBEAT_INTERVAL_SECS: u64 = 900;

    if config.heartbeat.interval_seconds < MIN_HEARTBEAT_INTERVAL_SECS {
        tracing::warn!(
            "heartbeat.interval_seconds={} is too low; clamping to {}",
            config.heartbeat.interval_seconds,
            MIN_HEARTBEAT_INTERVAL_SECS
        );
        config.heartbeat.interval_seconds = MIN_HEARTBEAT_INTERVAL_SECS;
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
    if let Ok(val) = env::var("JOSSIE_LLM_OPENAI_OPTIMIZATIONS") {
        if let Some(parsed) = parse_env_bool(&val) {
            config.llm.openai_optimizations = parsed;
        }
    }
    if let Ok(val) = env::var("JOSSIE_LLM_MAX_CONTEXT_CHARS") {
        if let Ok(parsed) = val.parse::<usize>() {
            config.llm.max_context_chars = parsed;
        }
    }
    if let Ok(val) = env::var("JOSSIE_LLM_CONTEXT_COMPACT_TARGET_CHARS") {
        if let Ok(parsed) = val.parse::<usize>() {
            config.llm.context_compact_target_chars = parsed;
        }
    }
    if let Ok(val) = env::var("JOSSIE_LLM_CONTEXT_KEEP_RECENT_DIALOGUE_MESSAGES") {
        if let Ok(parsed) = val.parse::<usize>() {
            config.llm.context_keep_recent_dialogue_messages = parsed;
        }
    }
    if let Ok(val) = env::var("JOSSIE_LLM_TRANSCRIPTION_MODEL") {
        config.llm.transcription_model = Some(val);
    }
    if let Ok(val) = env::var("JOSSIE_LLM_MAX_ATTACHMENT_BYTES_PER_REQUEST") {
        if let Ok(parsed) = val.parse::<usize>() {
            config.llm.max_attachment_bytes_per_request = parsed;
        }
    }
    if let Ok(val) = env::var("JOSSIE_TELEGRAM_BOT_TOKEN") {
        config.telegram.bot_token = val.trim().to_string();
    }
    if let Ok(val) = env::var("JOSSIE_TELEGRAM_MAX_DOWNLOAD_BYTES") {
        if let Ok(parsed) = val.parse::<usize>() {
            config.telegram.max_download_bytes = parsed;
        }
    }
    if let Ok(val) = env::var("JOSSIE_TELEGRAM_FFMPEG_PATH") {
        config.telegram.ffmpeg_path = val;
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
