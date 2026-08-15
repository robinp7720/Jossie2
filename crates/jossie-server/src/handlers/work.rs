use crate::{errors::AppError, events::ServerEvent, state::AppState};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use jossie_core::types::{Message, Role};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct WorkQuery {
    pub conversation_id: Option<Uuid>,
    pub before: Option<String>,
    pub limit: Option<usize>,
    pub include_quiet: Option<bool>,
    pub include_archived: Option<bool>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct WorkSummary {
    pub goals: Vec<jossie_db::GoalWithTasks>,
    pub active_runs: Vec<jossie_db::WorkRun>,
    pub recent_runs: Vec<jossie_db::WorkRun>,
    pub workers: Vec<jossie_db::WorkerStatus>,
    pub scheduled_tasks: Vec<jossie_db::ScheduledTask>,
    pub chat_imports: Vec<jossie_db::ChatImport>,
}

pub async fn work_summary(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WorkQuery>,
) -> Result<Json<WorkSummary>, AppError> {
    let (goals, active_runs, recent_runs, workers, scheduled_tasks, chat_imports) = tokio::try_join!(
        state.db.list_goals(query.include_archived.unwrap_or(false)),
        state.db.list_active_work_runs(query.conversation_id),
        state.db.list_work_runs(
            query.conversation_id,
            !query.include_quiet.unwrap_or(false),
            query.limit.unwrap_or(30),
            query.before.as_deref(),
        ),
        state.db.list_worker_statuses(),
        state.db.list_upcoming_scheduled_tasks(50),
        state.db.list_recent_chat_imports(20),
    )?;
    Ok(Json(WorkSummary {
        goals,
        active_runs,
        recent_runs,
        workers,
        scheduled_tasks,
        chat_imports,
    }))
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct GoalDetail {
    #[serde(flatten)]
    pub goal: jossie_db::GoalWithTasks,
    pub runs: Vec<jossie_db::WorkRun>,
}

pub async fn goal_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<GoalDetail>, AppError> {
    let goal = state
        .db
        .get_goal_with_tasks(&id)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Goal not found")))?;
    let runs = state.db.list_work_runs_for_goal(&id, 50).await?;
    Ok(Json(GoalDetail { goal, runs }))
}

#[derive(Debug, Deserialize)]
pub struct UpdateGoalRequest {
    pub title: Option<String>,
    pub archived: Option<bool>,
}

pub async fn update_goal(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateGoalRequest>,
) -> Result<Json<jossie_db::GoalWithTasks>, AppError> {
    if request
        .title
        .as_ref()
        .is_some_and(|title| title.trim().is_empty())
    {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "Goal title cannot be empty"
        )));
    }
    let updated = state
        .db
        .update_goal_metadata(
            &id,
            request.title.as_deref().map(str::trim),
            None,
            None,
            None,
            request.archived,
        )
        .await?;
    if !updated {
        return Err(AppError::not_found(anyhow::anyhow!("Goal not found")));
    }
    publish_goal(&state, &id).await
}

pub async fn pause_goal(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<jossie_db::GoalWithTasks>, AppError> {
    control_goal(&state, &id, "pause").await
}

pub async fn resume_goal(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<jossie_db::GoalWithTasks>, AppError> {
    control_goal(&state, &id, "resume").await
}

pub async fn cancel_goal(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<jossie_db::GoalWithTasks>, AppError> {
    control_goal(&state, &id, "cancel").await
}

async fn control_goal(
    state: &Arc<AppState>,
    id: &str,
    action: &str,
) -> Result<Json<jossie_db::GoalWithTasks>, AppError> {
    let goal = state
        .db
        .get_goal(id)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Goal not found")))?;
    let continuing_active = action == "resume" && goal.status == "active";
    if continuing_active {
        let already_running = state
            .db
            .list_work_runs_for_goal(id, 20)
            .await?
            .into_iter()
            .any(|run| {
                matches!(
                    run.status.as_str(),
                    "queued" | "running" | "waiting_for_approval"
                )
            });
        if already_running {
            return Err(AppError::bad_request(anyhow::anyhow!(
                "Goal is already being worked on"
            )));
        }
    } else if !state.db.set_goal_control_state(id, action).await? {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "Goal cannot transition from its current state"
        )));
    }
    if matches!(action, "pause" | "cancel")
        && let Some(conversation_id) = goal
            .conversation_id
            .and_then(|id| Uuid::parse_str(&id).ok())
    {
        state.request_cancel(conversation_id).await;
    }
    if action == "resume" {
        let goal_with_tasks = state
            .db
            .get_goal_with_tasks(id)
            .await?
            .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Goal not found")))?;
        let has_linked_schedule = goal_with_tasks
            .tasks
            .iter()
            .any(|task| task.source_type.as_deref() == Some("scheduled_task"));
        if !has_linked_schedule
            && let Some(conversation_id) = goal_with_tasks
                .goal
                .conversation_id
                .as_deref()
                .and_then(|value| Uuid::parse_str(value).ok())
        {
            let checkpoint = state.db.latest_available_checkpoint_for_goal(id).await?;
            let state = Arc::clone(state);
            let goal_id = id.to_string();
            let task_id = goal_with_tasks
                .tasks
                .iter()
                .find(|task| !matches!(task.status.as_str(), "completed" | "cancelled"))
                .map(|task| task.id.clone());
            let title = goal_with_tasks.goal.title.clone();
            tokio::spawn(async move {
                let message = Message {
                    id: Uuid::new_v4(),
                    conversation_id,
                    role: Role::User,
                    content: format!("Continue the tracked goal: {title}"),
                    tool_calls: None,
                    tool_call_id: None,
                    name: Some("goal_resume".to_string()),
                    attachments: None,
                    response_items: None,
                    created_at: chrono::Utc::now(),
                };
                if let Err(error) = crate::events::persist_message(&state, &message).await {
                    tracing::warn!("Failed to queue resumed goal {goal_id}: {error}");
                    return;
                }
                let options = crate::agent::AgentRunOptions {
                    goal_id: Some(goal_id.clone()),
                    task_id,
                    work_summary: Some(title),
                    resume_checkpoint_run_id: checkpoint.map(|item| item.run_id),
                    ..crate::agent::AgentRunOptions::default()
                };
                if let Err(error) =
                    crate::agent::run_agent_loop_when_available(&state, conversation_id, options)
                        .await
                {
                    tracing::warn!("Resumed goal {goal_id} did not complete: {error}");
                    let _ = state
                        .db
                        .update_goal_metadata(
                            &goal_id,
                            None,
                            None,
                            Some("paused"),
                            Some(Some(
                                "Resume attempt could not start; use Continue now to retry",
                            )),
                            None,
                        )
                        .await;
                }
            });
        }
    }
    publish_goal(state, id).await
}

async fn publish_goal(
    state: &Arc<AppState>,
    id: &str,
) -> Result<Json<jossie_db::GoalWithTasks>, AppError> {
    let goal = state
        .db
        .get_goal_with_tasks(id)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Goal not found")))?;
    if let Some(conversation_id) = goal
        .goal
        .conversation_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
    {
        state.publish_event(ServerEvent::GoalUpdated {
            conversation_id,
            goal: goal.clone(),
        });
    }
    Ok(Json(goal))
}

pub async fn run_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<jossie_db::WorkRunDetail>, AppError> {
    Ok(Json(state.db.get_work_run_detail(&id).await?.ok_or_else(
        || AppError::not_found(anyhow::anyhow!("Work run not found")),
    )?))
}

#[derive(Debug, Deserialize)]
pub struct WorkRunsQuery {
    pub conversation_id: Option<Uuid>,
    pub before: Option<String>,
    pub limit: Option<usize>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub include_quiet: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct WorkRunsResponse {
    pub items: Vec<jossie_db::WorkRun>,
    pub next_cursor: Option<String>,
}

pub async fn list_runs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WorkRunsQuery>,
) -> Result<Json<WorkRunsResponse>, AppError> {
    let limit = query.limit.unwrap_or(30).clamp(1, 100);
    let items = state
        .db
        .list_work_runs_filtered(
            query.conversation_id,
            !query.include_quiet.unwrap_or(false),
            limit,
            query.before.as_deref(),
            query.kind.as_deref(),
            query.status.as_deref(),
        )
        .await?;
    let next_cursor = (items.len() == limit)
        .then(|| items.last().map(|run| run.updated_at.clone()))
        .flatten();
    Ok(Json(WorkRunsResponse { items, next_cursor }))
}

pub async fn cancel_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<jossie_db::WorkRun>, AppError> {
    let run = state
        .db
        .get_work_run(&id)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Work run not found")))?;
    if !state.db.request_work_run_cancel(&id).await? {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "Run is no longer active"
        )));
    }
    if matches!(run.status.as_str(), "queued" | "waiting_for_approval") {
        state
            .db
            .reject_pending_actions_for_run(&id, "Work run cancelled")
            .await?;
        state
            .db
            .update_work_run(&id, "cancelled", Some("Cancelled"), None)
            .await?;
    }
    if let Some(conversation_id) = run
        .conversation_id
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
    {
        state.request_cancel(conversation_id).await;
    }
    Ok(Json(
        state
            .db
            .get_work_run(&id)
            .await?
            .expect("run existed before update"),
    ))
}
