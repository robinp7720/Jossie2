use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};

use crate::errors::AppError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct GraphQuery {
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct GraphResponse {
    pub nodes: Vec<jossie_db::GraphNode>,
    pub edges: Vec<jossie_db::GraphEdge>,
}

pub async fn graph_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GraphQuery>,
) -> Result<Json<GraphResponse>, AppError> {
    let limit = query.limit.unwrap_or(500);
    let nodes = state.db.graph_list_nodes(limit).await?;
    let edges = state.db.graph_list_edges(limit).await?;

    Ok(Json(GraphResponse { nodes, edges }))
}
