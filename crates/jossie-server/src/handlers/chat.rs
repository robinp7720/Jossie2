use crate::agent::{run_agent_loop, run_agent_loop_streaming};
use crate::errors::AppError;
use crate::events::persist_message;
use crate::state::AppState;
use axum::{
    Json,
    extract::{Query, State, WebSocketUpgrade, ws},
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
    persist_message(&state, &user_msg).await?;

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

#[derive(Deserialize)]
pub struct EventsWsQuery {
    pub conversation_id: Option<Uuid>,
}

pub async fn events_ws_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EventsWsQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_events_ws(state, socket, query.conversation_id))
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
        if persist_message(&state, &user_msg).await.is_err() {
            continue;
        }

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(100);
        let loop_state = state.clone();
        tokio::spawn(async move {
            run_agent_loop_streaming(&loop_state, conv_id, event_tx).await;
        });

        while let Some(event) = event_rx.recv().await {
            let ws_msg = match serde_json::to_value(event) {
                Ok(v) => v,
                Err(e) => serde_json::json!({"type": "error", "error": e.to_string()}),
            };
            let _ = socket
                .send(ws::Message::Text(ws_msg.to_string().into()))
                .await;
        }
    }
}

async fn handle_events_ws(
    state: Arc<AppState>,
    mut socket: ws::WebSocket,
    conversation_filter: Option<Uuid>,
) {
    let mut rx = state.subscribe_events();

    loop {
        tokio::select! {
            event = rx.recv() => {
                let Ok(event) = event else {
                    continue;
                };
                if let Some(filter) = conversation_filter {
                    if event.conversation_id() != filter {
                        continue;
                    }
                }

                let payload = match serde_json::to_string(&event) {
                    Ok(payload) => payload,
                    Err(e) => {
                        tracing::warn!("Failed to serialize server event: {e}");
                        continue;
                    }
                };

                if socket.send(ws::Message::Text(payload.into())).await.is_err() {
                    break;
                }
            }
            maybe_msg = futures::StreamExt::next(&mut socket) => {
                match maybe_msg {
                    Some(Ok(ws::Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}
