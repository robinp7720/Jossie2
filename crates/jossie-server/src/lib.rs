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
    routing::{delete, get, patch, post},
};
pub use events::ServerEvent;
pub use state::AppState;
use std::sync::Arc;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

pub fn router(state: Arc<AppState>) -> Router {
    let cors = if state.cors_origins.is_empty() {
        CorsLayer::new()
            .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
    } else {
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(
                state
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
            get(handlers::conversations::list_conversations),
        )
        .route(
            "/api/conversations/{id}/messages",
            get(handlers::conversations::get_messages),
        )
        .route(
            "/api/conversations/{id}/cancel",
            post(handlers::conversations::cancel_conversation_run),
        )
        // Files
        .route("/api/files", post(handlers::files::upload_file))
        .route("/api/graph", get(handlers::graph::graph_handler))
        .route(
            "/api/dashboard",
            get(handlers::dashboard::dashboard_handler),
        )
        .route("/api/memories", get(handlers::dashboard::memories_handler))
        .route("/api/activity", get(handlers::dashboard::activity_handler))
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
            "/setup/google",
            get(handlers::integrations::setup_google_handler),
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
        .route("/api/health", get(handlers::health::health_handler))
        .with_state(state.clone());

    let static_files = ServeDir::new("frontend/dist");

    Router::new()
        .merge(public)
        .merge(setup)
        .merge(api)
        .fallback_service(static_files)
        .layer(DefaultBodyLimit::max(state.max_request_body_bytes))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}
