use crate::agent::run_agent_loop;
use crate::errors::AppError;
use crate::events::{ServerEvent, persist_message};
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, Query, State},
};
use jossie_core::types::{Message, Role};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct PendingActionQuery {
    conversation_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionDecisionResponse {
    action_id: String,
    status: String,
}

#[derive(Debug, Clone)]
pub struct DeferredActionDecision {
    pub response: ActionDecisionResponse,
    pub conversation_id: Uuid,
    pub batch_id: String,
    pub batch_resolved: bool,
}

pub async fn list_pending_actions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PendingActionQuery>,
) -> Result<Json<Vec<jossie_db::PendingAction>>, AppError> {
    Ok(Json(
        state.db.list_pending_actions(query.conversation_id).await?,
    ))
}

fn resume_batch_when_ready(state: Arc<AppState>, batch_id: String, conversation_id: Uuid) {
    tokio::spawn(async move {
        match state.db.pending_action_batch_is_resolved(&batch_id).await {
            Ok(true) => {
                if let Err(error) = run_agent_loop(&state, conversation_id).await {
                    tracing::error!(
                        "Failed to resume conversation {conversation_id} after action batch {batch_id}: {error}"
                    );
                }
            }
            Ok(false) => {}
            Err(error) => tracing::error!("Failed to inspect action batch {batch_id}: {error}"),
        }
    });
}

pub async fn approve_action(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ActionDecisionResponse>, AppError> {
    decide_action(state, id, true).await.map(Json)
}

pub async fn reject_action(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ActionDecisionResponse>, AppError> {
    decide_action(state, id, false).await.map(Json)
}

pub async fn decide_action(
    state: Arc<AppState>,
    id: String,
    approve: bool,
) -> Result<ActionDecisionResponse, AppError> {
    let outcome = decide_action_deferred(state.clone(), id, approve).await?;
    if outcome.batch_resolved {
        resume_batch_when_ready(state, outcome.batch_id, outcome.conversation_id);
    }
    Ok(outcome.response)
}

pub async fn decide_action_deferred(
    state: Arc<AppState>,
    id: String,
    approve: bool,
) -> Result<DeferredActionDecision, AppError> {
    let action = match state.db.claim_pending_action(&id).await? {
        Some(action) => action,
        None => {
            let existing = state
                .db
                .get_pending_action(&id)
                .await?
                .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Action not found")))?;
            if matches!(
                existing.status.as_str(),
                "completed" | "failed" | "rejected" | "uncertain"
            ) {
                return Ok(DeferredActionDecision {
                    response: ActionDecisionResponse {
                        action_id: existing.id,
                        status: existing.status,
                    },
                    conversation_id: existing.conversation_id,
                    batch_id: existing.batch_id,
                    // This request did not resolve anything, so callers must not
                    // resume an already-finished batch a second time.
                    batch_resolved: false,
                });
            }
            return Err(AppError::conflict(anyhow::anyhow!(
                "Action is already executing"
            )));
        }
    };

    let terminal_status = if approve {
        let call = jossie_core::ToolCall {
            id: action.call_id.clone(),
            name: action.tool_name.clone(),
            arguments: action.arguments.clone(),
        };
        state
            .publish_durable_event(ServerEvent::ToolStarted {
                conversation_id: action.conversation_id,
                run_id: action.run_id.clone(),
                call_id: action.call_id.clone(),
                tool: action.tool_name.clone(),
            })
            .await;
        let result = state.registry.execute(&call).await;
        let status = if result.is_error {
            "failed"
        } else {
            "completed"
        };
        let error = result.is_error.then_some(result.content.as_str());
        let tool_message = Message::new(action.conversation_id, Role::Tool, result.content.clone())
            .with_tool_call_id(action.call_id.clone())
            .with_name(action.tool_name.clone());
        persist_message(&state, &tool_message).await?;
        state.db.resolve_pending_action(&id, status, error).await?;
        state
            .publish_durable_event(ServerEvent::ToolFinished {
                conversation_id: action.conversation_id,
                run_id: action.run_id.clone(),
                call_id: action.call_id.clone(),
                tool: action.tool_name.clone(),
                result_preview: crate::events::preview_text(&result.content, 220),
                is_error: result.is_error,
            })
            .await;
        status
    } else {
        let output = "The user did not approve this action. Do not attempt it again unless they make a new explicit request.";
        let tool_message = Message::new(action.conversation_id, Role::Tool, output.to_string())
            .with_tool_call_id(action.call_id.clone())
            .with_name(action.tool_name.clone());
        persist_message(&state, &tool_message).await?;
        state
            .db
            .resolve_pending_action(&id, "rejected", None)
            .await?;
        "rejected"
    };

    state
        .publish_durable_event(ServerEvent::ActionResolved {
            conversation_id: action.conversation_id,
            run_id: action.run_id.clone(),
            action_id: action.id.clone(),
            status: terminal_status.to_string(),
            title: action.title.clone(),
        })
        .await;
    let batch_resolved = state
        .db
        .pending_action_batch_is_resolved(&action.batch_id)
        .await?;

    Ok(DeferredActionDecision {
        response: ActionDecisionResponse {
            action_id: action.id,
            status: terminal_status.to_string(),
        },
        conversation_id: action.conversation_id,
        batch_id: action.batch_id,
        batch_resolved,
    })
}
