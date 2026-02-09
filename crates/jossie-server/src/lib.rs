pub mod agent;
pub mod errors;
pub mod handlers;
pub mod middleware;
pub mod state;

pub use agent::{prepend_system_prompt, run_agent_loop};
use axum::{
    Router,
    http::{Method, header},
    middleware as axum_middleware,
    routing::{delete, get, post},
};
pub use state::AppState;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

pub fn router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
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

    let public = Router::new().route(
        "/oauth/callback",
        get(handlers::integrations::oauth_callback_handler),
    );

    let static_files = ServeDir::new("frontend/dist");

    Router::new()
        .merge(public)
        .merge(setup)
        .merge(api)
        .fallback_service(static_files)
        .layer(cors)
        .with_state(state)
}
