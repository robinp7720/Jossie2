use std::sync::Arc;
use axum::{
    Router,
    routing::{get, post},
    extract::{State, WebSocketUpgrade, Path, ws},
    response::{IntoResponse, Response},
    http::{StatusCode, HeaderMap},
    Json,
    middleware::{self, Next},
};
use jossie_core::integration::IntegrationRegistry;
use jossie_core::types::{Message, Role};
use jossie_db::Database;
use jossie_llm::LlmClient;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::Utc;

pub struct AppState {
    pub db: Arc<Database>,
    pub llm: LlmClient,
    pub registry: IntegrationRegistry,
    pub auth_token: String,
    pub system_prompt: String,
    pub max_agent_iterations: usize,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/chat", post(chat_handler))
        .route("/api/chat/stream", get(ws_handler))
        .route("/api/conversations", get(list_conversations))
        .route("/api/conversations/{id}/messages", get(get_messages))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
}

// -- Error type for JSON error responses --

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

struct AppError(anyhow::Error);

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!("{:#}", self.0);
        let body = ErrorBody { error: self.0.to_string() };
        (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
    }
}

// -- Auth middleware --

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, Response> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match token {
        Some(t) if t == state.auth_token => Ok(next.run(request).await),
        _ => {
            let body = ErrorBody { error: "unauthorized".to_string() };
            Err((StatusCode::UNAUTHORIZED, Json(body)).into_response())
        }
    }
}

// -- Request / Response types --

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
    #[serde(default)]
    conversation_id: Option<Uuid>,
}

#[derive(Serialize)]
struct ChatResponse {
    conversation_id: Uuid,
    message: String,
}

// -- Handlers --

async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    let conv_id = match req.conversation_id {
        Some(id) => id,
        None => state.db.create_conversation(None).await?.id,
    };

    let user_msg = Message {
        id: Uuid::new_v4(),
        conversation_id: conv_id,
        role: Role::User,
        content: req.message,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        created_at: Utc::now(),
    };
    state.db.save_message(&user_msg).await?;

    let response = run_agent_loop(&state, conv_id).await?;

    Ok(Json(ChatResponse { conversation_id: conv_id, message: response }))
}

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(state, socket))
}

async fn handle_ws(state: Arc<AppState>, mut socket: ws::WebSocket) {
    while let Some(Ok(msg)) = futures::StreamExt::next(&mut socket).await {
        let ws::Message::Text(text) = msg else { continue };
        let Ok(req) = serde_json::from_str::<ChatRequest>(&text) else { continue };

        let conv_id = match req.conversation_id {
            Some(id) => id,
            None => match state.db.create_conversation(None).await {
                Ok(c) => c.id,
                Err(_) => continue,
            },
        };

        let user_msg = Message {
            id: Uuid::new_v4(),
            conversation_id: conv_id,
            role: Role::User,
            content: req.message,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            created_at: Utc::now(),
        };
        if state.db.save_message(&user_msg).await.is_err() { continue; }

        let tools = state.registry.all_tool_definitions();
        let mut messages = state.db.get_messages(conv_id).await.unwrap_or_default();
        prepend_system_prompt(&state.system_prompt, &mut messages);

        let max_iters = state.max_agent_iterations;
        for iteration in 0..max_iters {
            let (content, tool_calls) = match state.llm.complete(&messages, &tools).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = socket.send(ws::Message::Text(
                        serde_json::json!({"type": "error", "error": e.to_string()}).to_string().into()
                    )).await;
                    break;
                }
            };

            if !tool_calls.is_empty() {
                if iteration + 1 >= max_iters {
                    let _ = socket.send(ws::Message::Text(
                        serde_json::json!({"type": "error", "error": "Max agent iterations reached"}).to_string().into()
                    )).await;
                    break;
                }

                let tc_json = serde_json::to_value(&tool_calls).ok();
                let assistant_msg = Message {
                    id: Uuid::new_v4(),
                    conversation_id: conv_id,
                    role: Role::Assistant,
                    content: content.clone(),
                    tool_calls: tc_json,
                    tool_call_id: None,
                    name: None,
                    created_at: Utc::now(),
                };
                let _ = state.db.save_message(&assistant_msg).await;
                messages.push(assistant_msg);

                for call in &tool_calls {
                    let result = state.registry.execute(call).await;
                    let _ = socket.send(ws::Message::Text(
                        serde_json::json!({"type": "tool_result", "tool": call.name, "result": result.content}).to_string().into()
                    )).await;
                    let tool_msg = Message {
                        id: Uuid::new_v4(),
                        conversation_id: conv_id,
                        role: Role::Tool,
                        content: result.content,
                        tool_calls: None,
                        tool_call_id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                        created_at: Utc::now(),
                    };
                    let _ = state.db.save_message(&tool_msg).await;
                    messages.push(tool_msg);
                }
                continue;
            }

            let _ = socket.send(ws::Message::Text(
                serde_json::json!({"type": "message", "conversation_id": conv_id, "content": content}).to_string().into()
            )).await;
            let assistant_msg = Message {
                id: Uuid::new_v4(),
                conversation_id: conv_id,
                role: Role::Assistant,
                content,
                tool_calls: None,
                tool_call_id: None,
                name: None,
                created_at: Utc::now(),
            };
            let _ = state.db.save_message(&assistant_msg).await;
            break;
        }
    }
}

// -- Agent loop --

pub fn prepend_system_prompt(system_prompt: &str, messages: &mut Vec<Message>) {
    if system_prompt.is_empty() {
        return;
    }
    let sys_msg = Message {
        id: Uuid::nil(),
        conversation_id: Uuid::nil(),
        role: Role::System,
        content: system_prompt.to_string(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        created_at: Utc::now(),
    };
    messages.insert(0, sys_msg);
}

pub async fn run_agent_loop(state: &AppState, conv_id: Uuid) -> anyhow::Result<String> {
    let tools = state.registry.all_tool_definitions();
    let mut messages = state.db.get_messages(conv_id).await?;
    prepend_system_prompt(&state.system_prompt, &mut messages);

    for _iteration in 0..state.max_agent_iterations {
        let (content, tool_calls) = state.llm.complete(&messages, &tools).await?;

        if tool_calls.is_empty() {
            let msg = Message {
                id: Uuid::new_v4(),
                conversation_id: conv_id,
                role: Role::Assistant,
                content: content.clone(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                created_at: Utc::now(),
            };
            state.db.save_message(&msg).await?;
            return Ok(content);
        }

        let tc_json = serde_json::to_value(&tool_calls)?;
        let assistant_msg = Message {
            id: Uuid::new_v4(),
            conversation_id: conv_id,
            role: Role::Assistant,
            content: content.clone(),
            tool_calls: Some(tc_json),
            tool_call_id: None,
            name: None,
            created_at: Utc::now(),
        };
        state.db.save_message(&assistant_msg).await?;
        messages.push(assistant_msg);

        for call in &tool_calls {
            let result = state.registry.execute(call).await;
            let tool_msg = Message {
                id: Uuid::new_v4(),
                conversation_id: conv_id,
                role: Role::Tool,
                content: result.content,
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
                name: Some(call.name.clone()),
                created_at: Utc::now(),
            };
            state.db.save_message(&tool_msg).await?;
            messages.push(tool_msg);
        }
    }

    anyhow::bail!("Agent loop exceeded maximum of {} iterations", state.max_agent_iterations)
}

// -- Read-only endpoints --

async fn list_conversations(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<jossie_core::types::Conversation>>, AppError> {
    Ok(Json(state.db.list_conversations().await?))
}

async fn get_messages(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Message>>, AppError> {
    Ok(Json(state.db.get_messages(id).await?))
}
