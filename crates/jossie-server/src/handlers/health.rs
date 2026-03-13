use crate::state::AppState;
use axum::{Json, extract::State};
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub db: &'static str,
}

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let db_ok = state.db.health_check().await;

    Json(HealthResponse {
        status: if db_ok { "ok" } else { "degraded" },
        db: if db_ok { "connected" } else { "error" },
    })
}
