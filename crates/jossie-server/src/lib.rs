use std::sync::Arc;
use axum::{
    Router,
    routing::{get, post},
    extract::{State, WebSocketUpgrade, Path, ws, Query},
    response::{IntoResponse, Response, Html},
    http::{StatusCode, HeaderMap},
    Json,
    middleware::{self, Next},
};
use jossie_core::integration::{IntegrationRegistry, OnboardingStatus};
use jossie_core::types::{Message, Role};
use jossie_db::Database;
use jossie_llm::LlmClient;
use jossie_integration_google::GoogleIntegration;
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
    pub google_config: jossie_core::config::GoogleConfig,
}

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/api/chat", post(chat_handler))
        .route("/api/chat/stream", get(ws_handler))
        .route("/api/conversations", get(list_conversations))
        .route("/api/conversations/{id}/messages", get(get_messages))
        .route("/api/onboarding", get(onboarding_status_handler))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state.clone());

    // Web UI and Setup routes
    let setup = Router::new()
        .route("/setup/google", get(setup_google_handler))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    let public = Router::new()
        .route("/", get(index_handler))
        .route("/oauth/callback", get(oauth_callback_handler));

    Router::new()
        .merge(public)
        .merge(setup)
        .merge(api)
        .with_state(state)
}

// -- Onboarding --

#[derive(Serialize)]
struct IntegrationStatus {
    name: String,
    #[serde(flatten)]
    status: OnboardingStatus,
}

async fn onboarding_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<IntegrationStatus>>, AppError> {
    let mut statuses = Vec::new();
    for integration in state.registry.get_integrations() {
        let status = integration.check_onboarding().await?;
        statuses.push(IntegrationStatus {
            name: integration.name().to_string(),
            status,
        });
    }
    Ok(Json(statuses))
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
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| {
            request.uri().query().and_then(|q| {
                q.split('&').find_map(|pair| {
                    let mut split = pair.split('=');
                    if split.next() == Some("token") {
                        split.next()
                    } else {
                        None
                    }
                })
            })
        });

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
        prepend_system_prompt(&state, &mut messages).await;

        let max_iters = state.max_agent_iterations;
        for iteration in 0..max_iters {
            let (tx, mut rx) = tokio::sync::mpsc::channel(100);
            let llm = state.llm.clone();
            let messages_clone = messages.clone();
            let tools_clone = tools.clone();

            tokio::spawn(async move {
                if let Err(e) = llm.complete_stream(&messages_clone, &tools_clone, tx).await {
                    tracing::error!("LLM stream error: {e}");
                }
            });

            let mut full_content = String::new();
            let mut tool_calls = Vec::new();

            while let Some(event) = rx.recv().await {
                match event {
                    jossie_llm::StreamEvent::Delta(delta) => {
                        full_content.push_str(&delta);
                        let _ = socket.send(ws::Message::Text(
                            serde_json::json!({"type": "delta", "content": delta}).to_string().into()
                        )).await;
                    }
                    jossie_llm::StreamEvent::ToolCalls(calls) => {
                        tool_calls = calls;
                    }
                    jossie_llm::StreamEvent::Done => {
                        // Stream finished
                    }
                    jossie_llm::StreamEvent::Error(e) => {
                        let _ = socket.send(ws::Message::Text(
                            serde_json::json!({"type": "error", "error": e}).to_string().into()
                        )).await;
                    }
                }
            }
            
            // Send done message after stream ends
            let _ = socket.send(ws::Message::Text(
                serde_json::json!({"type": "done", "conversation_id": conv_id}).to_string().into()
            )).await;

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
                    content: full_content.clone(),
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

            let assistant_msg = Message {
                id: Uuid::new_v4(),
                conversation_id: conv_id,
                role: Role::Assistant,
                content: full_content,
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

async fn build_system_prompt(state: &AppState) -> String {
    let mut prompt = state.system_prompt.clone();

    // Dynamically append agent and user profiles from memory
    if let Ok(Some(entry)) = state.db.get_memory("agent_profile").await {
        prompt.push_str("\n\n## Agent Description (Jossie)\n");
        prompt.push_str(&entry.content);
    }
    
    if let Ok(Some(entry)) = state.db.get_memory("user_profile").await {
        prompt.push_str("\n\n## User Description\n");
        prompt.push_str(&entry.content);
    }

    prompt
}

pub async fn prepend_system_prompt(state: &AppState, messages: &mut Vec<Message>) {
    let content = build_system_prompt(state).await;
    if content.is_empty() {
        return;
    }
    let sys_msg = Message {
        id: Uuid::nil(),
        conversation_id: Uuid::nil(),
        role: Role::System,
        content,
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
    prepend_system_prompt(state, &mut messages).await;

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

// -- Web UI --

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

// -- Google Setup --

async fn setup_google_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> axum::response::Redirect {
    let host = headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or("localhost:3000");
    // Assuming HTTP for local setup. If behind HTTPS proxy, this might break, but good enough for onboarding.
    let redirect_uri = format!("http://{}/oauth/callback", host);
    
    let url = GoogleIntegration::generate_auth_url(&state.google_config, &redirect_uri);
    axum::response::Redirect::to(&url)
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    error: Option<String>,
}

async fn oauth_callback_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> impl IntoResponse {
    if let Some(error) = query.error {
        return Html(format!("<h1>Google Auth Error</h1><p>{}</p>", error));
    }
    
    let Some(code) = query.code else {
        return Html("<h1>Error</h1><p>No code received.</p>".to_string());
    };

    let host = headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or("localhost:3000");
    let redirect_uri = format!("http://{}/oauth/callback", host);

    match GoogleIntegration::exchange_code(&state.google_config, &code, &redirect_uri).await {
        Ok(token) => {
            if let Err(e) = state.db.set_integration_setting("google", "refresh_token", &token).await {
                return Html(format!("<h1>Error Saving Token</h1><p>{}</p>", e));
            }
            Html(format!(
                r#"
                <h1>Success!</h1>
                <p>Google integration configured successfully.</p>
                <p>You can close this window.</p>
                <script>setTimeout(() => window.close(), 3000);</script>
                "#
            ))
        },
        Err(e) => Html(format!("<h1>Exchange Error</h1><p>{}</p>", e)),
    }
}
