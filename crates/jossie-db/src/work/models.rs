use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkRunStatus {
    Queued,
    Running,
    WaitingForApproval,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Paused,
}

impl WorkRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingForApproval => "waiting_for_approval",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Paused => "paused",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted | Self::Paused
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, ts_rs::TS)]
pub struct Goal {
    pub id: String,
    pub conversation_id: Option<String>,
    pub title: String,
    pub objective: String,
    pub status: String,
    pub blocker: Option<String>,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, ts_rs::TS)]
pub struct GoalTask {
    pub id: String,
    pub goal_id: String,
    pub position: i64,
    pub title: String,
    pub status: String,
    pub summary: Option<String>,
    pub blocker: Option<String>,
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct GoalWithTasks {
    #[serde(flatten)]
    pub goal: Goal,
    pub tasks: Vec<GoalTask>,
    pub completed_tasks: usize,
    pub total_tasks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, ts_rs::TS)]
pub struct WorkRun {
    pub id: String,
    pub goal_id: Option<String>,
    pub task_id: Option<String>,
    pub conversation_id: Option<String>,
    pub kind: String,
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub status: String,
    pub summary: String,
    pub current_phase: Option<String>,
    pub error: Option<String>,
    pub visibility: String,
    pub cancel_requested: bool,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, ts_rs::TS)]
pub struct WorkRunStep {
    pub id: String,
    pub run_id: String,
    pub sequence: i64,
    pub kind: String,
    pub label: String,
    pub status: String,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct WorkRunDetail {
    #[serde(flatten)]
    pub run: WorkRun,
    pub steps: Vec<WorkRunStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkRunCheckpoint {
    pub run_id: String,
    pub goal_id: String,
    pub task_id: Option<String>,
    pub conversation_id: String,
    pub state_json: String,
    pub partial_response: String,
    pub status: String,
    pub resumed_by_run_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, ts_rs::TS)]
pub struct WorkerStatus {
    pub worker_key: String,
    pub label: String,
    pub status: String,
    pub current_run_id: Option<String>,
    pub detail: Option<String>,
    pub last_started_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct NewWorkRun<'a> {
    pub id: Option<&'a str>,
    pub goal_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub conversation_id: Option<Uuid>,
    pub kind: &'a str,
    pub source_type: Option<&'a str>,
    pub source_id: Option<&'a str>,
    pub summary: &'a str,
    pub visibility: &'a str,
}
