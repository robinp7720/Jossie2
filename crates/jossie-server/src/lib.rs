pub mod agent;
pub mod errors;
pub mod handlers;
pub mod middleware;
pub mod state;

pub use agent::{prepend_system_prompt, run_agent_loop};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, Method, header},
    middleware as axum_middleware,
    routing::{delete, get, post},
};
pub use state::AppState;
use std::sync::Arc;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

pub fn router(state: Arc<AppState>) -> Router {
    let allow_origin = if state.cors_origins.is_empty() {
        AllowOrigin::any()
    } else {
        AllowOrigin::list(
            state
                .cors_origins
                .iter()
                .filter_map(|o| o.parse::<HeaderValue>().ok()),
        )
    };

    let cors = CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    let api = Router::new()
        // Chat
        .route("/api/chat", post(handlers::chat::chat_handler))
        .route("/api/chat/stream", get(handlers::chat::ws_handler))
        // Conversations
        .route(
            "/api/conversations",
            get(handlers::conversations::list_conversations),
        )
        .route(
            "/api/conversations/{id}/messages",
            get(handlers::conversations::get_messages),
        )
        .route("/api/graph", get(handlers::graph::graph_handler))
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
            delete(handlers::config::delete_account),
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
