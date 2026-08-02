use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use jossie_core::integration::IntegrationRegistry;
use jossie_db::Database;
use jossie_llm::LlmClient;
use jossie_server::{AppState, router};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tower::ServiceExt;

async fn setup_app() -> axum::Router {
    let db = Database::new("sqlite::memory:").await.unwrap();
    db.migrate().await.unwrap();

    let (event_tx, _) = broadcast::channel(100);

    let state = Arc::new(AppState {
        db: Arc::new(db),
        llm: LlmClient::new("http://localhost:8080", "key", "model"),
        kg_llm: LlmClient::new("http://localhost:8080", "key", "model"),
        registry: Arc::new(IntegrationRegistry::new()),
        auth_token: "test-token".to_string(),
        auth_password_hash: test_password_hash(),
        session_cookie_secure: false,
        public_base_url: None,
        system_prompt: "test prompt".to_string(),
        max_agent_iterations: 5,
        max_context_messages: 10,
        event_max_context_messages: 10,
        google_config: jossie_core::config::GoogleConfig {
            client_id: "".to_string(),
            client_secret: "".to_string(),
            refresh_token: "".to_string(),
            debug_gmail_payload: false,
        },
        google_integration: None,
        telegram_token: "".to_string(),
        enable_self_reflection: false,
        active_conversations: Arc::new(RwLock::new(HashSet::new())),
        cancelled_conversations: Arc::new(RwLock::new(HashSet::new())),
        pending_google_oauth: Arc::new(RwLock::new(HashMap::new())),
        event_tx,
        cors_origins: vec![],
        max_request_body_bytes: 1024 * 1024,
    });

    router(state)
}

fn test_password_hash() -> String {
    let salt = SaltString::encode_b64(b"jossie-test-salt").unwrap();
    Argon2::default()
        .hash_password(b"correct-horse", &salt)
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn test_health_check() {
    let app = setup_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn test_auth_middleware_unauthorized() {
    let app = setup_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/conversations")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_auth_middleware_authorized() {
    let app = setup_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/conversations")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn pending_actions_are_authenticated_and_empty_by_default() {
    let app = setup_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/actions/pending")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        serde_json::json!([])
    );
}

#[tokio::test]
async fn approving_an_unknown_action_is_not_found() {
    let app = setup_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/actions/not-pending/approve")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_auth_middleware_query_token() {
    let app = setup_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/conversations?token=test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn login_creates_a_cookie_session_for_protected_routes() {
    let app = setup_app().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"password":"correct-horse"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("HttpOnly"));
    let cookie_pair = cookie.split(';').next().unwrap().to_string();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/conversations")
                .header("Cookie", cookie_pair)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn dashboard_is_available_to_authenticated_clients() {
    let app = setup_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/dashboard")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["stats"]["memories"], 0);
}

#[tokio::test]
async fn test_chat_rejects_unknown_conversation_id() {
    let app = setup_app().await;
    let missing_id = uuid::Uuid::new_v4();
    let payload = serde_json::json!({
        "message": "hello",
        "conversation_id": missing_id,
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("Authorization", "Bearer test-token")
                .header("Content-Type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "Conversation not found");
}
