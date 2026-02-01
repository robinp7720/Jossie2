use std::sync::Arc;
use axum::{
    extract::{State, Path, Query},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;
use jossie_core::types::Message;
use crate::state::AppState;
use crate::errors::AppError;

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
