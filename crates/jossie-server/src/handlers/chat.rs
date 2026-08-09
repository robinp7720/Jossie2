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
    #[serde(default)]
    client_message_id: Option<Uuid>,
}

#[derive(Serialize)]
pub struct ChatResponse {
    conversation_id: Uuid,
    message: String,
}

fn attachments_match(message: &Message, requested_file_ids: Option<&[Uuid]>) -> bool {
    let mut stored = message
        .attachments
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|attachment| attachment.id)
        .collect::<Vec<_>>();
    let mut requested = requested_file_ids.unwrap_or_default().to_vec();
    stored.sort_unstable();
    requested.sort_unstable();
    stored == requested
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

#[derive(Clone, Copy)]
pub enum PendingReply {
    Approve,
    Reject,
}

pub fn pending_reply(message: &str, action_count: usize) -> Option<PendingReply> {
    let normalized = message
        .trim()
        .to_lowercase()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_string();
    let approve = [
        "yes",
        "yes do it",
        "approve",
        "approve it",
        "go ahead",
        "do it",
    ];
    let reject = [
        "no",
        "no thanks",
        "reject",
        "reject it",
        "don't do it",
        "cancel it",
    ];
    if action_count == 1 && approve.contains(&normalized.as_str()) || normalized == "approve all" {
        Some(PendingReply::Approve)
    } else if action_count == 1 && reject.contains(&normalized.as_str())
        || normalized == "reject all"
    {
        Some(PendingReply::Reject)
    } else {
        None
    }
}

async fn resolve_pending_reply(
    state: &Arc<AppState>,
    conversation_id: Uuid,
    message: &str,
) -> Result<Option<String>, AppError> {
    let actions = state.db.list_pending_actions(Some(conversation_id)).await?;
    let actionable = actions
        .into_iter()
        .filter(|action| action.status == "pending")
        .collect::<Vec<_>>();
    if actionable.is_empty() {
        return Ok(None);
    }
    let Some(decision) = pending_reply(message, actionable.len()) else {
        return Err(AppError::conflict(anyhow::anyhow!(
            "This conversation has a pending action. Approve or reject it before sending another request."
        )));
    };
    for action in actionable {
        crate::handlers::actions::decide_action(
            state.clone(),
            action.id,
            matches!(decision, PendingReply::Approve),
        )
        .await?;
    }
    Ok(Some(match decision {
        PendingReply::Approve => "Approved. Jossie is continuing the run.".to_string(),
        PendingReply::Reject => "Rejected. Jossie is continuing without that action.".to_string(),
    }))
}

pub async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    let conv_id = get_or_create_conversation_id(&state, req.conversation_id).await?;

    if let Some(message_id) = req.client_message_id
        && let Some(existing) = state.db.get_message(message_id).await?
    {
        if existing.conversation_id != conv_id
            || existing.role != Role::User
            || existing.content != req.message
            || !attachments_match(&existing, req.file_ids.as_deref())
        {
            return Err(AppError::conflict(anyhow::anyhow!(
                "client_message_id is already used by a different message"
            )));
        }
        return Ok(Json(ChatResponse {
            conversation_id: conv_id,
            message: "Message already accepted".to_string(),
        }));
    }

    if let Some(message) = resolve_pending_reply(&state, conv_id, &req.message).await? {
        return Ok(Json(ChatResponse {
            conversation_id: conv_id,
            message,
        }));
    }

    let mut user_msg = Message::new(conv_id, Role::User, req.message);
    if let Some(message_id) = req.client_message_id {
        user_msg.id = message_id;
    }
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
                    data: None,
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

        let existing = match req.client_message_id {
            Some(message_id) => state.db.get_message(message_id).await.ok().flatten(),
            None => None,
        };
        if let Some(existing) = existing.as_ref()
            && (existing.conversation_id != conv_id
                || existing.role != Role::User
                || existing.content != req.message
                || !attachments_match(existing, req.file_ids.as_deref()))
        {
            let payload = serde_json::json!({
                "type": "error",
                "conversation_id": conv_id,
                "error": "client_message_id is already used by a different message",
            });
            let _ = socket
                .send(ws::Message::Text(payload.to_string().into()))
                .await;
            continue;
        }

        match if existing.is_some() {
            Ok::<Option<String>, AppError>(None)
        } else {
            resolve_pending_reply(&state, conv_id, &req.message).await
        } {
            Ok(Some(message)) => {
                let payload = serde_json::json!({
                    "type": "action_decision_received",
                    "conversation_id": conv_id,
                    "message": message,
                });
                let _ = socket
                    .send(ws::Message::Text(payload.to_string().into()))
                    .await;
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                let payload = serde_json::json!({
                    "type": "pending_action",
                    "conversation_id": conv_id,
                    "error": error.to_string(),
                });
                let _ = socket
                    .send(ws::Message::Text(payload.to_string().into()))
                    .await;
                continue;
            }
        }

        let mut user_msg = Message::new(conv_id, Role::User, req.message);
        if let Some(message_id) = req.client_message_id {
            user_msg.id = message_id;
        }
        if let Some(ref fids) = req.file_ids {
            let mut attachments = Vec::new();
            for fid in fids {
                if let Ok(Some(record)) = state.db.get_file_record(fid).await {
                    attachments.push(jossie_core::types::Attachment {
                        id: record.id,
                        name: record.name,
                        mime_type: record.mime_type,
                        size: record.size,
                        data: None,
                    });
                }
            }
            user_msg = user_msg.with_attachments(attachments);
        }
        let duplicate = existing.is_some();
        if !duplicate && persist_message(&state, &user_msg).await.is_err() {
            let payload = serde_json::json!({
                "type": "error",
                "conversation_id": conv_id,
                "error": "Failed to save message",
            });
            let _ = socket
                .send(ws::Message::Text(payload.to_string().into()))
                .await;
            continue;
        }

        // Link attachments in DB
        if !duplicate && let Some(fids) = req.file_ids {
            for fid in fids {
                let _ = state.db.link_message_attachment(user_msg.id, fid).await;
            }
        }

        let source_id = user_msg.id.to_string();
        let existing_run = state
            .db
            .get_work_run_by_source("chat_message", &source_id)
            .await
            .ok()
            .flatten();
        let proposed_run_id = Uuid::new_v4().to_string();
        let run_id = if let Some(run) = existing_run {
            run.id
        } else {
            match state
                .db
                .create_work_run(jossie_db::NewWorkRun {
                    id: Some(&proposed_run_id),
                    goal_id: None,
                    task_id: None,
                    conversation_id: Some(conv_id),
                    kind: "chat",
                    source_type: Some("chat_message"),
                    source_id: Some(&source_id),
                    summary: "Conversation request",
                    visibility: "significant",
                })
                .await
            {
                Ok(run) => run.id,
                Err(_) => match state
                    .db
                    .get_work_run_by_source("chat_message", &source_id)
                    .await
                    .ok()
                    .flatten()
                {
                    Some(run) => run.id,
                    None => {
                        let payload = serde_json::json!({
                            "type": "error",
                            "conversation_id": conv_id,
                            "error": "Failed to create work run",
                        });
                        let _ = socket
                            .send(ws::Message::Text(payload.to_string().into()))
                            .await;
                        continue;
                    }
                },
            }
        };
        let should_spawn = match state.db.claim_queued_work_run(&run_id).await {
            Ok(claimed) => claimed,
            Err(error) => {
                tracing::error!(%error, %run_id, "failed to claim queued chat work run");
                let payload = serde_json::json!({
                    "type": "error",
                    "conversation_id": conv_id,
                    "error": "Failed to start work run",
                });
                let _ = socket
                    .send(ws::Message::Text(payload.to_string().into()))
                    .await;
                continue;
            }
        };
        let accepted = serde_json::json!({
            "type": "message_accepted",
            "conversation_id": conv_id,
            "message_id": user_msg.id,
            "duplicate": duplicate,
            "run_id": run_id.clone(),
        });
        if !should_spawn {
            let _ = socket
                .send(ws::Message::Text(accepted.to_string().into()))
                .await;
            continue;
        }

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(100);
        let loop_state = state.clone();
        let source_message_id = Some(user_msg.id);
        let run_id_for_task = run_id.clone();
        tokio::spawn(async move {
            run_agent_loop_streaming(
                &loop_state,
                conv_id,
                run_id_for_task,
                source_message_id,
                event_tx,
            )
            .await;
        });
        if socket
            .send(ws::Message::Text(accepted.to_string().into()))
            .await
            .is_err()
        {
            continue;
        }

        while let Some(event) = event_rx.recv().await {
            let ws_msg = match serde_json::to_value(event) {
                Ok(v) => v,
                Err(e) => serde_json::json!({"type": "error", "error": e.to_string()}),
            };
            if socket
                .send(ws::Message::Text(ws_msg.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
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
                    if event.conversation_id().is_some_and(|conversation_id| conversation_id != filter) {
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
