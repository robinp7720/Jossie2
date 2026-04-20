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
    #[serde(default)]
    file_ids: Option<Vec<Uuid>>,
}

#[derive(Serialize)]
pub struct ChatResponse {
    conversation_id: Uuid,
    message: String,
}

async fn conversation_exists(state: &Arc<AppState>, conversation_id: Uuid) -> anyhow::Result<bool> {
    Ok(state.db.get_conversation(conversation_id).await?.is_some())
}

async fn get_or_create_conversation_id(
    state: &Arc<AppState>,
    conversation_id: Option<Uuid>,
) -> Result<Uuid, AppError> {
    match conversation_id {
        Some(id) => {
            if conversation_exists(state, id).await? {
                Ok(id)
            } else {
                Err(AppError::not_found(anyhow::anyhow!(
                    "Conversation not found"
                )))
            }
        }
        None => Ok(state.db.create_conversation(None).await?.id),
    }
}

pub async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    let conv_id = get_or_create_conversation_id(&state, req.conversation_id).await?;

    let mut user_msg = Message::new(conv_id, Role::User, req.message);
    if let Some(ref fids) = req.file_ids {
        let mut attachments = Vec::new();
        for fid in fids {
            if let Some(record) = state
                .db
                .get_file_record(fid)
                .await
                .map_err(anyhow::Error::from)?
            {
                attachments.push(jossie_core::types::Attachment {
                    id: record.id,
                    name: record.name,
                    mime_type: record.mime_type,
                    size: record.size,
                });
            }
        }
        user_msg = user_msg.with_attachments(attachments);
    }
    persist_message(&state, &user_msg).await?;

    // Link attachments in DB
    if let Some(fids) = req.file_ids {
        for fid in fids {
            state
                .db
                .link_message_attachment(user_msg.id, fid)
                .await
                .map_err(anyhow::Error::from)?;
        }
    }

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
            Some(id) => match conversation_exists(&state, id).await {
                Ok(true) => id,
                Ok(false) => {
                    let error = serde_json::json!({
                        "type": "error",
                        "error": "Conversation not found",
                    });
                    let _ = socket
                        .send(ws::Message::Text(error.to_string().into()))
                        .await;
                    continue;
                }
                Err(e) => {
                    tracing::error!("Failed to load conversation {}: {}", id, e);
                    let error = serde_json::json!({
                        "type": "error",
                        "error": "Failed to load conversation",
                    });
                    let _ = socket
                        .send(ws::Message::Text(error.to_string().into()))
                        .await;
                    continue;
                }
            },
            None => match state.db.create_conversation(None).await {
                Ok(c) => c.id,
                Err(e) => {
                    tracing::error!("Failed to create conversation: {}", e);
                    let error = serde_json::json!({
                        "type": "error",
                        "error": "Failed to create conversation",
                    });
                    let _ = socket
                        .send(ws::Message::Text(error.to_string().into()))
                        .await;
                    continue;
                }
            },
        };

        tracing::info!("Processing message for conversation {}", conv_id);

        let mut user_msg = Message::new(conv_id, Role::User, req.message);
        if let Some(ref fids) = req.file_ids {
            let mut attachments = Vec::new();
            for fid in fids {
                if let Ok(Some(record)) = state.db.get_file_record(fid).await {
                    attachments.push(jossie_core::types::Attachment {
                        id: record.id,
                        name: record.name,
                        mime_type: record.mime_type,
                        size: record.size,
                    });
                }
            }
            user_msg = user_msg.with_attachments(attachments);
        }
        if persist_message(&state, &user_msg).await.is_err() {
            continue;
        }

        // Link attachments in DB
        if let Some(fids) = req.file_ids {
            for fid in fids {
                let _ = state.db.link_message_attachment(user_msg.id, fid).await;
            }
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
