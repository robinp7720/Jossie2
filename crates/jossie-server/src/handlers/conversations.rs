use crate::errors::AppError;
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, Query, State},
};
use jossie_core::types::Message;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub async fn list_conversations(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<jossie_core::types::Conversation>>, AppError> {
    Ok(Json(state.db.list_conversations().await?))
}

#[derive(Deserialize)]
pub struct GetMessagesParams {
    limit: Option<usize>,
}

pub async fn get_messages(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<GetMessagesParams>,
) -> Result<Json<Vec<Message>>, AppError> {
    Ok(Json(state.db.get_messages(id, params.limit).await?))
}

#[derive(Serialize)]
pub struct CancelRunResponse {
    pub conversation_id: Uuid,
    pub status: &'static str,
}

pub async fn cancel_conversation_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<CancelRunResponse>, AppError> {
    state.request_cancel(id).await;
    Ok(Json(CancelRunResponse {
        conversation_id: id,
        status: "cancel_requested",
    }))
}
