pub mod agent;
pub mod errors;
pub mod events;
pub mod handlers;
pub mod middleware;
pub mod state;

pub use agent::{prepend_system_prompt, run_agent_loop};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, Method, header},
    middleware as axum_middleware,
    routing::{get, patch, post},
};
pub use events::ServerEvent;
pub use state::{
    AgentRuntimeConfig, AppState, BackgroundRuntimeConfig, TelegramRuntimeConfig, WebRuntimeConfig,
};
use std::sync::Arc;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

pub fn router(state: Arc<AppState>) -> Router {
    let cors = if state.web.cors_origins.is_empty() {
        CorsLayer::new()
            .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
    } else {
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(
                state
                    .web
                    .cors_origins
                    .iter()
                    .filter_map(|o| o.parse::<HeaderValue>().ok()),
            ))
            .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
            .allow_credentials(true)
    };

    let api = Router::new()
        .route("/api/auth/logout", post(handlers::auth::logout_handler))
        // Chat
        .route("/api/chat", post(handlers::chat::chat_handler))
        .route("/api/chat/stream", get(handlers::chat::ws_handler))
        .route("/api/events", get(handlers::chat::events_ws_handler))
        // Conversations
        .route(
            "/api/conversations",
            get(handlers::conversations::list_conversations)
                .post(handlers::conversations::create_conversation),
        )
        .route(
            "/api/conversations/{id}",
            patch(handlers::conversations::update_conversation)
                .delete(handlers::conversations::delete_conversation),
        )
        .route(
            "/api/conversations/{id}/messages",
            get(handlers::conversations::get_messages),
        )
        .route(
            "/api/conversations/{id}/export",
            get(handlers::conversations::export_conversation),
        )
        .route(
            "/api/conversations/{id}/cancel",
            post(handlers::conversations::cancel_conversation_run),
        )
        // Files
        .route("/api/files", post(handlers::files::upload_file))
        .route(
            "/api/files/{id}",
            get(handlers::files::download_file).delete(handlers::files::delete_file),
        )
        .route(
            "/api/chat-imports",
            post(handlers::files::start_chat_import),
        )
        .route(
            "/api/chat-imports/{id}",
            get(handlers::files::get_chat_import),
        )
        .route("/api/graph", get(handlers::graph::graph_handler))
        .route(
            "/api/dashboard",
            get(handlers::dashboard::dashboard_handler),
        )
        .route("/api/memories", get(handlers::dashboard::memories_handler))
        .route("/api/activity", get(handlers::dashboard::activity_handler))
        .route("/api/work", get(handlers::work::work_summary))
        .route(
            "/api/goals/{id}",
            get(handlers::work::goal_detail).patch(handlers::work::update_goal),
        )
        .route("/api/goals/{id}/pause", post(handlers::work::pause_goal))
        .route("/api/goals/{id}/resume", post(handlers::work::resume_goal))
        .route("/api/goals/{id}/cancel", post(handlers::work::cancel_goal))
        .route("/api/work/runs/{id}", get(handlers::work::run_detail))
        .route("/api/work/runs", get(handlers::work::list_runs))
        .route(
            "/api/work/runs/{id}/cancel",
            post(handlers::work::cancel_run),
        )
        .route(
            "/api/actions/pending",
            get(handlers::actions::list_pending_actions),
        )
        .route(
            "/api/actions/{id}/approve",
            post(handlers::actions::approve_action),
        )
        .route(
            "/api/actions/{id}/reject",
            post(handlers::actions::reject_action),
        )
        // Config / Onboarding
        .route(
            "/api/onboarding",
            get(handlers::integrations::onboarding_status_handler),
        )
        .route(
            "/api/config/accounts",
            get(handlers::config::list_accounts).post(handlers::config::add_account),
        )
        .route(
            "/api/config/integration-types",
            get(handlers::config::list_integration_types),
        )
        .route(
            "/api/config/accounts/{id}",
            patch(handlers::config::update_account).delete(handlers::config::delete_account),
        )
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ))
        // Limit concurrent API requests to prevent resource exhaustion
        .layer(ConcurrencyLimitLayer::new(64))
        .with_state(state.clone());

    // Setup routes (Auth protected)
    let setup = Router::new()
        .route(
            "/setup/{provider}",
            get(handlers::integrations::setup_provider_handler),
        )
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ));

    let public = Router::new()
        .route("/api/auth/login", post(handlers::auth::login_handler))
        .route("/api/auth/session", get(handlers::auth::session_handler))
        .route(
            "/oauth/callback",
            get(handlers::integrations::oauth_callback_handler),
        )
        .route(
            "/api/integrations/webhooks/{provider}",
            post(handlers::integrations::webhook_handler),
        )
        .route("/api/health", get(handlers::health::health_handler))
        .with_state(state.clone());

    let static_files = ServeDir::new("frontend/dist");

    Router::new()
        .merge(public)
        .merge(setup)
        .merge(api)
        .fallback_service(static_files)
        .layer(DefaultBodyLimit::max(state.web.max_request_body_bytes))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}
