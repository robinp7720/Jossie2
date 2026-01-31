use std::sync::Arc;
use axum::{
    extract::{State, WebSocketUpgrade, ws},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::Utc;
use jossie_core::types::{Message, Role};
use crate::state::AppState;
use crate::errors::AppError;
use crate::agent::{run_agent_loop, prepend_system_prompt};

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

pub async fn ws_handler(
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
