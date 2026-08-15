use crate::errors::AppError;
use crate::state::AppState;
use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, ts_rs::TS)]
pub struct DashboardResponse {
    pub stats: DashboardStats,
    pub recent_memories: Vec<jossie_db::MemoryEntryWithMetadata>,
    pub recent_activity: Vec<jossie_db::ActivityEvent>,
    pub recent_conversations: Vec<jossie_core::Conversation>,
    pub upcoming_tasks: Vec<jossie_db::ScheduledTask>,
    pub graph_highlights: Vec<GraphHighlight>,
}

#[derive(Serialize, ts_rs::TS)]
pub struct DashboardStats {
    pub memories: i64,
    pub prompt_ready_memories: i64,
    pub knowledge_nodes: i64,
    pub knowledge_edges: i64,
    pub pending_tasks: usize,
    pub active_goals: usize,
    pub active_runs: usize,
    pub waiting_work: usize,
    pub blocked_goals: usize,
}

#[derive(Serialize, ts_rs::TS)]
pub struct GraphHighlight {
    pub node: jossie_db::GraphNode,
    pub connections: i64,
}

pub async fn dashboard_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<DashboardResponse>, AppError> {
    let (
        memory_stats,
        graph_counts,
        recent_memories,
        recent_activity,
        recent_conversations,
        upcoming_tasks,
        graph_nodes,
        goals,
        active_runs,
    ) = tokio::try_join!(
        state.db.memory_stats(),
        state.db.graph_counts(),
        state.db.memory_list_all(5),
        state.db.list_activity_events(8, None),
        state.db.list_conversations(),
        state.db.list_upcoming_scheduled_tasks(4),
        state.db.graph_central_nodes(5),
        state.db.list_goals(false),
        state.db.list_active_work_runs(None),
    )?;

    Ok(Json(DashboardResponse {
        stats: DashboardStats {
            memories: memory_stats.total,
            prompt_ready_memories: memory_stats.prompt_ready,
            knowledge_nodes: graph_counts.0,
            knowledge_edges: graph_counts.1,
            pending_tasks: upcoming_tasks.len(),
            active_goals: goals
                .iter()
                .filter(|goal| matches!(goal.goal.status.as_str(), "active" | "blocked" | "paused"))
                .count(),
            active_runs: active_runs.len(),
            waiting_work: active_runs
                .iter()
                .filter(|run| run.status == "waiting_for_approval")
                .count(),
            blocked_goals: goals
                .iter()
                .filter(|goal| goal.goal.status == "blocked")
                .count(),
        },
        recent_memories,
        recent_activity,
        recent_conversations: recent_conversations.into_iter().take(5).collect(),
        upcoming_tasks,
        graph_highlights: graph_nodes
            .into_iter()
            .map(|(node, connections)| GraphHighlight { node, connections })
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct MemoryQuery {
    pub query: Option<String>,
    pub scope: Option<String>,
    pub limit: Option<usize>,
}

pub async fn memories_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<MemoryQuery>,
) -> Result<Json<Vec<jossie_db::MemoryEntryWithMetadata>>, AppError> {
    Ok(Json(
        state
            .db
            .memory_list_for_dashboard(
                query.query.as_deref(),
                query.scope.as_deref(),
                query.limit.unwrap_or(50),
            )
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct ActivityQuery {
    pub limit: Option<usize>,
    pub before: Option<String>,
}

#[derive(Serialize)]
pub struct ActivityResponse {
    pub items: Vec<jossie_db::ActivityEvent>,
    pub next_cursor: Option<String>,
}

pub async fn activity_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<ActivityResponse>, AppError> {
    let limit = query.limit.unwrap_or(30).clamp(1, 100);
    let items = state
        .db
        .list_activity_events(limit, query.before.as_deref())
        .await?;
    let next_cursor = (items.len() == limit)
        .then(|| items.last().map(|event| event.created_at.clone()))
        .flatten();
    Ok(Json(ActivityResponse { items, next_cursor }))
}
