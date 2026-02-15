use crate::agent::{AgentStreamEvent, run_agent_loop, run_agent_loop_streaming};
use crate::errors::AppError;
use crate::state::AppState;
use axum::{
    Json,
    extract::{State, WebSocketUpgrade, ws},
    response::IntoResponse,
};
use jossie_core::types::{Message, Role};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ChatRequest {
    message: String,
    #[serde(default)]
    conversation_id: Option<Uuid>,
}

#[derive(Serialize)]
pub struct ChatResponse {
    conversation_id: Uuid,
    message: String,
}

pub async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    let conv_id = match req.conversation_id {
        Some(id) => id,
        None => state.db.create_conversation(None).await?.id,
    };

    let user_msg = Message::new(conv_id, Role::User, req.message);
    state.db.save_message(&user_msg).await?;

    let response = run_agent_loop(&state, conv_id).await?;

    Ok(Json(ChatResponse {
        conversation_id: conv_id,
        message: response,
    }))
}

pub async fn ws_handler(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(state, socket))
}

async fn handle_ws(state: Arc<AppState>, mut socket: ws::WebSocket) {
    tracing::info!("WebSocket connection established");
    while let Some(Ok(msg)) = futures::StreamExt::next(&mut socket).await {
        let ws::Message::Text(text) = msg else {
            tracing::debug!("Received non-text message");
            continue;
        };
        tracing::debug!("Received WS message: {}", text);

        let req = match serde_json::from_str::<ChatRequest>(&text) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to parse ChatRequest: {}; text: {}", e, text);
                continue;
            }
        };

        let conv_id = match req.conversation_id {
            Some(id) => id,
            None => match state.db.create_conversation(None).await {
                Ok(c) => c.id,
                Err(e) => {
                    tracing::error!("Failed to create conversation: {}", e);
                    continue;
                }
            },
        };

        tracing::info!("Processing message for conversation {}", conv_id);

        let user_msg = Message::new(conv_id, Role::User, req.message);
        if state.db.save_message(&user_msg).await.is_err() {
            continue;
        }

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(100);
        let loop_state = state.clone();
        tokio::spawn(async move {
            run_agent_loop_streaming(&loop_state, conv_id, event_tx).await;
        });

        while let Some(event) = event_rx.recv().await {
            let ws_msg = match event {
                AgentStreamEvent::Delta(delta) => {
                    serde_json::json!({"type": "delta", "content": delta})
                }
                AgentStreamEvent::ToolResult { tool, result } => {
                    serde_json::json!({"type": "tool_result", "tool": tool, "result": result})
                }
                AgentStreamEvent::Done { conversation_id } => {
                    serde_json::json!({"type": "done", "conversation_id": conversation_id})
                }
                AgentStreamEvent::Error(e) => {
                    serde_json::json!({"type": "error", "error": e})
                }
            };
            let _ = socket
                .send(ws::Message::Text(ws_msg.to_string().into()))
                .await;
        }
    }
}
