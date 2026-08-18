use anyhow::Result;
use jossie_core::integration::IntegrationRegistry;
use jossie_db::{Database, WorkRunStatus};
use jossie_integration_http::HttpIntegration;
use jossie_integration_memory::MemoryIntegration;
use jossie_integration_personal::{
    HomeIntegration, NotionIntegration, SpotifyIntegration, TasksIntegration,
};
use jossie_integration_scheduler::SchedulerIntegration;
use jossie_llm::LlmClient;
use jossie_server::{
    AgentRuntimeConfig, AppState, BackgroundRuntimeConfig, TelegramRuntimeConfig, WebRuntimeConfig,
};
use std::sync::Arc;

mod config;
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

    let config = config::load("config.toml")?;

    tracing::info!("Connecting to database...");
    let db = Database::new_with_encryption_key(
        &config.database.url,
        (!config.database.encryption_key.trim().is_empty())
            .then_some(config.database.encryption_key.as_str()),
    )
    .await?;
    if config.database.encryption_key.trim().is_empty() {
        tracing::warn!(
            "database.encryption_key is unset; integration credentials are not encrypted at rest"
        );
    }
    db.migrate().await?;
    let interrupted_runs = db.mark_running_work_interrupted().await?;
    if interrupted_runs > 0 {
        tracing::warn!("Marked {interrupted_runs} interrupted work run(s) after restart");
    }
    let paused_goals = db.pause_goals_with_interrupted_runs().await?;
    if paused_goals > 0 {
        tracing::warn!("Paused {paused_goals} goal(s) whose active run was interrupted");
    }
    let recovered_checkpoints = db.create_checkpoints_for_interrupted_runs().await?;
    if recovered_checkpoints > 0 {
        tracing::warn!(
            "Created {recovered_checkpoints} recovery checkpoint(s) for interrupted legacy runs"
        );
    }
    let interrupted_schedules = db.mark_running_scheduled_tasks_interrupted().await?;
    if interrupted_schedules > 0 {
        tracing::warn!(
            "Marked {interrupted_schedules} interrupted scheduled task(s) as failed without retrying"
        );
    }
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

    let mut registry = IntegrationRegistry::with_max_output_chars(config.llm.max_tool_result_chars);
    registry.register(Arc::new(MemoryIntegration::new(db.clone())))?;
    registry.register(Arc::new(jossie_integration_files::FilesIntegration::new(
        db.clone(),
        chat_export_importer.clone(),
    )))?;

    // Knowledge Graph
    registry.register(Arc::new(jossie_integration_graph::GraphIntegration::new(
        db.clone(),
    )))?;

    let mut email = jossie_integration_email::EmailIntegration::new(&config.email);
    email.set_db(db.clone());
    let email = Arc::new(email);
    registry.register(email.clone())?;
    tracing::info!("Registered email integration");

    let mut google_integration: Option<Arc<jossie_integration_google::GoogleIntegration>> = None;
    if !config.google.client_id.is_empty() {
        let mut google = jossie_integration_google::GoogleIntegration::new(&config.google);
        google.set_db(db.clone());
        let google = Arc::new(google);
        registry.register(google.clone())?;
        google_integration = Some(google);
        tracing::info!("Registered Google integration");
    }

    let mail_integration = Arc::new(jossie_integration_mail::MailIntegration::new(
        email.clone(),
        google_integration.clone(),
    ));
    registry.register(mail_integration.clone())?;
    tracing::info!("Registered mail integration");

    registry.register(Arc::new(TasksIntegration::new(
        db.clone(),
        google_integration.clone(),
        &config.todoist,
    )))?;
    registry.register(Arc::new(HomeIntegration::new(db.clone())))?;
    registry.register(Arc::new(NotionIntegration::new(db.clone(), &config.notion)))?;
    registry.register(Arc::new(SpotifyIntegration::new(
        db.clone(),
        &config.spotify,
    )))?;
    tracing::info!("Registered personal integrations");

    // Browser Integration
    registry.register(Arc::new(
        jossie_integration_browser::BrowserIntegration::new(),
    ))?;
    tracing::info!("Registered browser integration");

    // HTTP Integration
    registry.register(Arc::new(HttpIntegration::new(
        config.http.allowed_domains.clone(),
    )))?;
    tracing::info!("Registered http integration");

    // Scheduler Integration
    registry.register(Arc::new(SchedulerIntegration::new(db.clone())))?;
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
        mail_integration,
        agent: AgentRuntimeConfig {
            system_prompt: config.llm.system_prompt.clone(),
            max_agent_iterations,
            max_context_messages: config.llm.max_context_messages,
            event_max_context_messages: config.llm.event_max_context_messages,
            openai_optimizations: config.llm.openai_optimizations,
            max_context_chars: config.llm.max_context_chars,
            context_compact_target_chars: config.llm.context_compact_target_chars,
            context_keep_recent_dialogue_messages: config.llm.context_keep_recent_dialogue_messages,
            interactive_run_budget_seconds: config.llm.interactive_run_budget_seconds,
            llm_request_timeout_seconds: config.llm.llm_request_timeout_seconds,
            tool_call_timeout_seconds: config.llm.tool_call_timeout_seconds,
            max_tool_batch_chars: config.llm.max_tool_batch_chars,
            max_attachment_bytes_per_request: config.llm.max_attachment_bytes_per_request,
            enable_self_reflection: config.llm.enable_self_reflection,
        },
        web: WebRuntimeConfig {
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
            cors_origins: config.server.cors_origins.clone(),
            max_request_body_bytes: config.server.max_request_body_bytes,
        },
        telegram: TelegramRuntimeConfig {
            token: config.telegram.bot_token.clone(),
            max_download_bytes: config.telegram.max_download_bytes,
            ffmpeg_path: config.telegram.ffmpeg_path.clone(),
        },
        background: BackgroundRuntimeConfig {
            heartbeat_enabled: config.heartbeat.enabled,
            heartbeat_interval_secs: config.heartbeat.interval_seconds,
        },
        active_conversations: Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
        cancelled_conversations: Arc::new(tokio::sync::RwLock::new(
            std::collections::HashSet::new(),
        )),
        run_cancellations: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        pending_oauth: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        event_tx,
    });

    tokio::spawn(run_work_watchdog(state.clone()));

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

async fn run_work_watchdog(state: Arc<AppState>) {
    let stale_after = std::cmp::max(
        state.agent.llm_request_timeout_seconds,
        state.agent.tool_call_timeout_seconds,
    ) + 30;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        interval.tick().await;
        let Ok(runs) = state.db.list_active_work_runs(None).await else {
            tracing::warn!("Work watchdog could not read active runs");
            continue;
        };
        let now = chrono::Utc::now();
        for run in runs {
            let Ok(updated_at) = chrono::DateTime::parse_from_rfc3339(&run.updated_at) else {
                continue;
            };
            if now
                .signed_duration_since(updated_at.with_timezone(&chrono::Utc))
                .num_seconds()
                <= stale_after as i64
            {
                continue;
            }
            tracing::error!(
                run_id = %run.id,
                conversation_id = run.conversation_id.as_deref().unwrap_or(""),
                stale_after_seconds = stale_after,
                "Work watchdog detected a stalled run"
            );
            let _ = state
                .db
                .update_work_run(
                    &run.id,
                    WorkRunStatus::Failed,
                    Some("Stopped after making no progress"),
                    Some("Run exceeded the stalled-operation deadline"),
                )
                .await;
            if let Some(conversation_id) = run
                .conversation_id
                .as_deref()
                .and_then(|value| uuid::Uuid::parse_str(value).ok())
            {
                state.request_cancel(conversation_id).await;
            }
        }
    }
}
