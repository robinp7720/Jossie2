use super::Database;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, QueryBuilder, Sqlite};
use uuid::Uuid;

pub const ACTIVE_RUN_STATUSES: &[&str] = &["queued", "running", "waiting_for_approval"];

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalWithTasks {
    #[serde(flatten)]
    pub goal: Goal,
    pub tasks: Vec<GoalTask>,
    pub completed_tasks: usize,
    pub total_tasks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkRunDetail {
    #[serde(flatten)]
    pub run: WorkRun,
    pub steps: Vec<WorkRunStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
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

impl Database {
    pub async fn create_goal(
        &self,
        conversation_id: Option<Uuid>,
        title: &str,
        objective: &str,
        tasks: &[String],
    ) -> anyhow::Result<GoalWithTasks> {
        let now = Utc::now().to_rfc3339();
        let goal_id = Uuid::new_v4().to_string();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO goals (id, conversation_id, title, objective, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, 'active', ?, ?)",
        )
        .bind(&goal_id)
        .bind(conversation_id.map(|id| id.to_string()))
        .bind(title)
        .bind(objective)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        for (position, task) in tasks.iter().enumerate() {
            sqlx::query(
                "INSERT INTO goal_tasks (id, goal_id, position, title, status, created_at, updated_at)
                 VALUES (?, ?, ?, ?, 'pending', ?, ?)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(&goal_id)
            .bind(position as i64)
            .bind(task)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.get_goal_with_tasks(&goal_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("created goal disappeared"))
    }

    pub async fn get_goal(&self, id: &str) -> anyhow::Result<Option<Goal>> {
        Ok(sqlx::query_as::<_, Goal>(
            "SELECT id, conversation_id, title, objective, status, blocker, archived_at, created_at, updated_at
             FROM goals WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn get_active_goal_for_conversation(
        &self,
        conversation_id: Uuid,
    ) -> anyhow::Result<Option<GoalWithTasks>> {
        let goal = sqlx::query_as::<_, Goal>(
            "SELECT id, conversation_id, title, objective, status, blocker, archived_at, created_at, updated_at
             FROM goals WHERE conversation_id = ? AND archived_at IS NULL AND status IN ('active','blocked','paused')
             ORDER BY updated_at DESC LIMIT 1",
        ).bind(conversation_id.to_string()).fetch_optional(&self.pool).await?;
        let Some(goal) = goal else { return Ok(None) };
        let tasks = self.list_goal_tasks(&goal.id).await?;
        let completed_tasks = tasks
            .iter()
            .filter(|task| task.status == "completed")
            .count();
        let total_tasks = tasks.len();
        Ok(Some(GoalWithTasks {
            goal,
            tasks,
            completed_tasks,
            total_tasks,
        }))
    }

    pub async fn link_work_run_goal(
        &self,
        run_id: &str,
        goal_id: &str,
        task_id: Option<&str>,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "UPDATE work_runs SET goal_id = ?, task_id = ?, updated_at = ? WHERE id = ?",
        )
        .bind(goal_id)
        .bind(task_id)
        .bind(Utc::now().to_rfc3339())
        .bind(run_id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn annotate_work_run(
        &self,
        run_id: &str,
        goal_id: Option<&str>,
        task_id: Option<&str>,
        source_type: Option<&str>,
        source_id: Option<&str>,
        summary: Option<&str>,
        visibility: Option<&str>,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().to_rfc3339();
        Ok(sqlx::query(
            "UPDATE work_runs SET goal_id = COALESCE(?, goal_id), task_id = COALESCE(?, task_id),
             source_type = COALESCE(?, source_type), source_id = COALESCE(?, source_id),
             summary = COALESCE(?, summary), visibility = COALESCE(?, visibility), updated_at = ? WHERE id = ?",
        ).bind(goal_id).bind(task_id).bind(source_type).bind(source_id).bind(summary).bind(visibility).bind(&now).bind(run_id)
        .execute(&self.pool).await?.rows_affected() == 1)
    }

    pub async fn list_goals(&self, include_archived: bool) -> anyhow::Result<Vec<GoalWithTasks>> {
        let goals = if include_archived {
            sqlx::query_as::<_, Goal>(
                "SELECT id, conversation_id, title, objective, status, blocker, archived_at, created_at, updated_at
                 FROM goals ORDER BY updated_at DESC",
            )
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, Goal>(
                "SELECT id, conversation_id, title, objective, status, blocker, archived_at, created_at, updated_at
                 FROM goals WHERE archived_at IS NULL ORDER BY
                 CASE status WHEN 'active' THEN 0 WHEN 'blocked' THEN 1 WHEN 'paused' THEN 2 ELSE 3 END,
                 updated_at DESC",
            )
            .fetch_all(&self.pool)
            .await?
        };
        let mut result = Vec::with_capacity(goals.len());
        for goal in goals {
            let tasks = self.list_goal_tasks(&goal.id).await?;
            let completed_tasks = tasks
                .iter()
                .filter(|task| task.status == "completed")
                .count();
            let total_tasks = tasks.len();
            result.push(GoalWithTasks {
                goal,
                tasks,
                completed_tasks,
                total_tasks,
            });
        }
        Ok(result)
    }

    pub async fn get_goal_with_tasks(&self, id: &str) -> anyhow::Result<Option<GoalWithTasks>> {
        let Some(goal) = self.get_goal(id).await? else {
            return Ok(None);
        };
        let tasks = self.list_goal_tasks(id).await?;
        let completed_tasks = tasks
            .iter()
            .filter(|task| task.status == "completed")
            .count();
        let total_tasks = tasks.len();
        Ok(Some(GoalWithTasks {
            goal,
            tasks,
            completed_tasks,
            total_tasks,
        }))
    }

    pub async fn list_goal_tasks(&self, goal_id: &str) -> anyhow::Result<Vec<GoalTask>> {
        Ok(sqlx::query_as::<_, GoalTask>(
            "SELECT id, goal_id, position, title, status, summary, blocker, source_type, source_id, created_at, updated_at
             FROM goal_tasks WHERE goal_id = ? ORDER BY position, created_at",
        )
        .bind(goal_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn update_goal_metadata(
        &self,
        id: &str,
        title: Option<&str>,
        objective: Option<&str>,
        status: Option<&str>,
        blocker: Option<Option<&str>>,
        archive: Option<bool>,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut builder = QueryBuilder::<Sqlite>::new("UPDATE goals SET updated_at = ");
        builder.push_bind(&now);
        if let Some(title) = title {
            builder.push(", title = ").push_bind(title);
        }
        if let Some(objective) = objective {
            builder.push(", objective = ").push_bind(objective);
        }
        if let Some(status) = status {
            builder.push(", status = ").push_bind(status);
        }
        if let Some(blocker) = blocker {
            builder.push(", blocker = ").push_bind(blocker);
        }
        if let Some(archive) = archive {
            builder
                .push(", archived_at = ")
                .push_bind(archive.then_some(now.clone()));
        }
        builder.push(" WHERE id = ").push_bind(id);
        Ok(builder.build().execute(&self.pool).await?.rows_affected() == 1)
    }

    pub async fn set_goal_control_state(&self, id: &str, action: &str) -> anyhow::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut tx = self.pool.begin().await?;
        let affected = match action {
            "pause" => {
                let rows = sqlx::query("UPDATE goals SET status = 'paused', blocker = NULL, updated_at = ? WHERE id = ? AND status IN ('active','blocked')")
                    .bind(&now).bind(id).execute(&mut *tx).await?.rows_affected();
                sqlx::query("UPDATE work_runs SET cancel_requested = 1, current_phase = 'Pausing safely', updated_at = ? WHERE goal_id = ? AND status IN ('queued','running','waiting_for_approval')")
                    .bind(&now).bind(id).execute(&mut *tx).await?;
                sqlx::query("UPDATE scheduled_tasks SET status = 'paused', updated_at = ? WHERE id IN (SELECT source_id FROM goal_tasks WHERE goal_id = ? AND source_type = 'scheduled_task') AND status = 'pending'")
                    .bind(&now).bind(id).execute(&mut *tx).await?;
                rows
            }
            "resume" => {
                let rows = sqlx::query("UPDATE goals SET status = 'active', blocker = NULL, updated_at = ? WHERE id = ? AND status IN ('paused','blocked')")
                    .bind(&now).bind(id).execute(&mut *tx).await?.rows_affected();
                sqlx::query("UPDATE scheduled_tasks SET status = 'pending', updated_at = ? WHERE id IN (SELECT source_id FROM goal_tasks WHERE goal_id = ? AND source_type = 'scheduled_task') AND status = 'paused'")
                    .bind(&now).bind(id).execute(&mut *tx).await?;
                rows
            }
            "cancel" => {
                let rows = sqlx::query("UPDATE goals SET status = 'cancelled', blocker = NULL, updated_at = ? WHERE id = ? AND status NOT IN ('completed','cancelled')")
                    .bind(&now).bind(id).execute(&mut *tx).await?.rows_affected();
                sqlx::query("UPDATE goal_tasks SET status = 'cancelled', updated_at = ? WHERE goal_id = ? AND status NOT IN ('completed','cancelled')")
                    .bind(&now).bind(id).execute(&mut *tx).await?;
                sqlx::query("UPDATE work_runs SET cancel_requested = 1, current_phase = 'Stopping safely', updated_at = ? WHERE goal_id = ? AND status IN ('queued','running','waiting_for_approval')")
                    .bind(&now).bind(id).execute(&mut *tx).await?;
                sqlx::query("UPDATE scheduled_tasks SET status = 'cancelled', updated_at = ? WHERE id IN (SELECT source_id FROM goal_tasks WHERE goal_id = ? AND source_type = 'scheduled_task') AND status IN ('pending','paused','running')")
                    .bind(&now).bind(id).execute(&mut *tx).await?;
                sqlx::query("UPDATE pending_actions SET status = 'rejected', result_error = 'Goal cancelled', resolved_at = ?, updated_at = ? WHERE run_id IN (SELECT id FROM work_runs WHERE goal_id = ?) AND status = 'pending'")
                    .bind(&now).bind(&now).bind(id).execute(&mut *tx).await?;
                rows
            }
            _ => anyhow::bail!("unsupported goal action: {action}"),
        };
        tx.commit().await?;
        Ok(affected == 1)
    }

    pub async fn link_goal_task_source(
        &self,
        task_id: &str,
        source_type: &str,
        source_id: &str,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "UPDATE goal_tasks SET source_type = ?, source_id = ?, updated_at = ? WHERE id = ?",
        )
        .bind(source_type)
        .bind(source_id)
        .bind(Utc::now().to_rfc3339())
        .bind(task_id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn list_work_runs_for_goal(
        &self,
        goal_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<WorkRun>> {
        Ok(sqlx::query_as::<_, WorkRun>(
            "SELECT id, goal_id, task_id, conversation_id, kind, source_type, source_id, status,
             summary, current_phase, error, visibility, cancel_requested, started_at, finished_at, created_at, updated_at
             FROM work_runs WHERE goal_id = ? ORDER BY updated_at DESC LIMIT ?",
        ).bind(goal_id).bind(limit.clamp(1, 100) as i64).fetch_all(&self.pool).await?)
    }

    pub async fn upsert_goal_task(
        &self,
        goal_id: &str,
        id: Option<&str>,
        position: i64,
        title: &str,
        status: &str,
        summary: Option<&str>,
        blocker: Option<&str>,
    ) -> anyhow::Result<GoalTask> {
        let now = Utc::now().to_rfc3339();
        let id = id
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        sqlx::query(
            "INSERT INTO goal_tasks (id, goal_id, position, title, status, summary, blocker, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET position = excluded.position, title = excluded.title,
             status = excluded.status, summary = excluded.summary, blocker = excluded.blocker,
             updated_at = excluded.updated_at WHERE goal_tasks.goal_id = excluded.goal_id",
        )
        .bind(&id).bind(goal_id).bind(position).bind(title).bind(status)
        .bind(summary).bind(blocker).bind(&now).bind(&now)
        .execute(&self.pool).await?;
        sqlx::query("UPDATE goals SET updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(goal_id)
            .execute(&self.pool)
            .await?;
        Ok(sqlx::query_as::<_, GoalTask>(
            "SELECT id, goal_id, position, title, status, summary, blocker, source_type, source_id, created_at, updated_at
             FROM goal_tasks WHERE id = ?",
        ).bind(&id).fetch_one(&self.pool).await?)
    }

    pub async fn create_work_run(&self, run: NewWorkRun<'_>) -> anyhow::Result<WorkRun> {
        let now = Utc::now().to_rfc3339();
        let id = run
            .id
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        sqlx::query(
            "INSERT INTO work_runs
             (id, goal_id, task_id, conversation_id, kind, source_type, source_id, status, summary, visibility, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?, ?, ?)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(&id).bind(run.goal_id).bind(run.task_id)
        .bind(run.conversation_id.map(|id| id.to_string())).bind(run.kind)
        .bind(run.source_type).bind(run.source_id).bind(run.summary).bind(run.visibility)
        .bind(&now).bind(&now).execute(&self.pool).await?;
        self.get_work_run(&id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("created work run disappeared"))
    }

    pub async fn get_work_run(&self, id: &str) -> anyhow::Result<Option<WorkRun>> {
        Ok(sqlx::query_as::<_, WorkRun>(
            "SELECT id, goal_id, task_id, conversation_id, kind, source_type, source_id, status,
             summary, current_phase, error, visibility, cancel_requested, started_at, finished_at, created_at, updated_at
             FROM work_runs WHERE id = ?",
        ).bind(id).fetch_optional(&self.pool).await?)
    }

    pub async fn get_work_run_detail(&self, id: &str) -> anyhow::Result<Option<WorkRunDetail>> {
        let Some(run) = self.get_work_run(id).await? else {
            return Ok(None);
        };
        let steps = self.list_work_run_steps(id).await?;
        Ok(Some(WorkRunDetail { run, steps }))
    }

    pub async fn list_work_runs(
        &self,
        conversation_id: Option<Uuid>,
        significant_only: bool,
        limit: usize,
        before: Option<&str>,
    ) -> anyhow::Result<Vec<WorkRun>> {
        self.list_work_runs_filtered(conversation_id, significant_only, limit, before, None, None)
            .await
    }

    pub async fn list_work_runs_filtered(
        &self,
        conversation_id: Option<Uuid>,
        significant_only: bool,
        limit: usize,
        before: Option<&str>,
        kind: Option<&str>,
        status: Option<&str>,
    ) -> anyhow::Result<Vec<WorkRun>> {
        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, goal_id, task_id, conversation_id, kind, source_type, source_id, status,
             summary, current_phase, error, visibility, cancel_requested, started_at, finished_at, created_at, updated_at
             FROM work_runs WHERE 1 = 1",
        );
        if let Some(conversation_id) = conversation_id {
            builder
                .push(" AND conversation_id = ")
                .push_bind(conversation_id.to_string());
        }
        if significant_only {
            builder.push(" AND visibility = 'significant'");
        }
        if let Some(before) = before {
            builder.push(" AND updated_at < ").push_bind(before);
        }
        if let Some(kind) = kind {
            builder.push(" AND kind = ").push_bind(kind);
        }
        if let Some(status) = status {
            builder.push(" AND status = ").push_bind(status);
        }
        builder
            .push(" ORDER BY updated_at DESC LIMIT ")
            .push_bind(limit.clamp(1, 100) as i64);
        Ok(builder
            .build_query_as::<WorkRun>()
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn list_active_work_runs(
        &self,
        conversation_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<WorkRun>> {
        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, goal_id, task_id, conversation_id, kind, source_type, source_id, status,
             summary, current_phase, error, visibility, cancel_requested, started_at, finished_at, created_at, updated_at
             FROM work_runs WHERE status IN ('queued', 'running', 'waiting_for_approval')",
        );
        if let Some(conversation_id) = conversation_id {
            builder
                .push(" AND conversation_id = ")
                .push_bind(conversation_id.to_string());
        }
        builder.push(" ORDER BY created_at");
        Ok(builder
            .build_query_as::<WorkRun>()
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn update_work_run(
        &self,
        id: &str,
        status: &str,
        phase: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let terminal = matches!(status, "completed" | "failed" | "cancelled" | "interrupted");
        let started = (status == "running").then_some(now.clone());
        let finished = terminal.then_some(now.clone());
        Ok(sqlx::query(
            "UPDATE work_runs SET status = ?, current_phase = ?, error = ?,
             started_at = COALESCE(started_at, ?), finished_at = COALESCE(?, finished_at), updated_at = ?
             WHERE id = ?",
        ).bind(status).bind(phase).bind(error).bind(started).bind(finished).bind(&now).bind(id)
        .execute(&self.pool).await?.rows_affected() == 1)
    }

    pub async fn request_work_run_cancel(&self, id: &str) -> anyhow::Result<bool> {
        Ok(sqlx::query("UPDATE work_runs SET cancel_requested = 1, current_phase = 'Stopping safely', updated_at = ? WHERE id = ? AND status IN ('queued','running','waiting_for_approval')")
            .bind(Utc::now().to_rfc3339()).bind(id).execute(&self.pool).await?.rows_affected() == 1)
    }

    pub async fn reject_pending_actions_for_run(
        &self,
        run_id: &str,
        reason: &str,
    ) -> anyhow::Result<u64> {
        let now = Utc::now().to_rfc3339();
        Ok(sqlx::query("UPDATE pending_actions SET status = 'rejected', result_error = ?, resolved_at = ?, updated_at = ? WHERE run_id = ? AND status = 'pending'")
            .bind(reason).bind(&now).bind(&now).bind(run_id).execute(&self.pool).await?.rows_affected())
    }

    pub async fn is_work_run_cancel_requested(&self, id: &str) -> anyhow::Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM work_runs WHERE id = ? AND cancel_requested = 1",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?
            > 0)
    }

    pub async fn create_work_run_step(
        &self,
        run_id: &str,
        step_id: Option<&str>,
        kind: &str,
        label: &str,
    ) -> anyhow::Result<WorkRunStep> {
        let now = Utc::now().to_rfc3339();
        let id = step_id
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let sequence = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM work_run_steps WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_one(&self.pool)
        .await?;
        sqlx::query("INSERT INTO work_run_steps (id, run_id, sequence, kind, label, status, started_at) VALUES (?, ?, ?, ?, ?, 'running', ?) ON CONFLICT(id) DO NOTHING")
            .bind(&id).bind(run_id).bind(sequence).bind(kind).bind(label).bind(&now).execute(&self.pool).await?;
        Ok(sqlx::query_as::<_, WorkRunStep>("SELECT id, run_id, sequence, kind, label, status, summary, error, started_at, finished_at FROM work_run_steps WHERE id = ?")
            .bind(&id).fetch_one(&self.pool).await?)
    }

    pub async fn finish_work_run_step(
        &self,
        id: &str,
        status: &str,
        summary: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query("UPDATE work_run_steps SET status = ?, summary = ?, error = ?, finished_at = ? WHERE id = ?")
            .bind(status).bind(summary).bind(error).bind(Utc::now().to_rfc3339()).bind(id)
            .execute(&self.pool).await?.rows_affected() == 1)
    }

    pub async fn complete_instant_work_run_step(
        &self,
        run_id: &str,
        kind: &str,
        label: &str,
        summary: Option<&str>,
    ) -> anyhow::Result<WorkRunStep> {
        let step = self.create_work_run_step(run_id, None, kind, label).await?;
        self.finish_work_run_step(&step.id, "completed", summary, None)
            .await?;
        Ok(sqlx::query_as::<_, WorkRunStep>("SELECT id, run_id, sequence, kind, label, status, summary, error, started_at, finished_at FROM work_run_steps WHERE id = ?")
            .bind(&step.id).fetch_one(&self.pool).await?)
    }

    pub async fn list_work_run_steps(&self, run_id: &str) -> anyhow::Result<Vec<WorkRunStep>> {
        Ok(sqlx::query_as::<_, WorkRunStep>("SELECT id, run_id, sequence, kind, label, status, summary, error, started_at, finished_at FROM work_run_steps WHERE run_id = ? ORDER BY sequence")
            .bind(run_id).fetch_all(&self.pool).await?)
    }

    pub async fn mark_running_work_interrupted(&self) -> anyhow::Result<u64> {
        Ok(sqlx::query("UPDATE work_runs SET status = 'interrupted', error = 'Interrupted by server restart', finished_at = ?, updated_at = ? WHERE status IN ('running','waiting_for_approval')")
            .bind(Utc::now().to_rfc3339()).bind(Utc::now().to_rfc3339()).execute(&self.pool).await?.rows_affected())
    }

    pub async fn upsert_worker_status(
        &self,
        key: &str,
        label: &str,
        status: &str,
        current_run_id: Option<&str>,
        detail: Option<&str>,
        success: bool,
        error: Option<&str>,
    ) -> anyhow::Result<WorkerStatus> {
        let now = Utc::now().to_rfc3339();
        let started_at = (status == "running").then_some(now.clone());
        let success_at = success.then_some(now.clone());
        let error_at = error.map(|_| now.clone());
        sqlx::query(
            "INSERT INTO worker_status (worker_key, label, status, current_run_id, detail, last_started_at, last_success_at, last_error_at, last_error, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(worker_key) DO UPDATE SET label = excluded.label, status = excluded.status,
             current_run_id = excluded.current_run_id, detail = excluded.detail,
             last_started_at = COALESCE(excluded.last_started_at, worker_status.last_started_at),
             last_success_at = COALESCE(excluded.last_success_at, worker_status.last_success_at),
             last_error_at = COALESCE(excluded.last_error_at, worker_status.last_error_at),
             last_error = CASE WHEN excluded.last_error IS NOT NULL THEN excluded.last_error WHEN excluded.last_success_at IS NOT NULL THEN NULL ELSE worker_status.last_error END,
             updated_at = excluded.updated_at",
        ).bind(key).bind(label).bind(status).bind(current_run_id).bind(detail).bind(started_at)
        .bind(success_at).bind(error_at).bind(error).bind(&now).execute(&self.pool).await?;
        Ok(sqlx::query_as::<_, WorkerStatus>("SELECT worker_key, label, status, current_run_id, detail, last_started_at, last_success_at, last_error_at, last_error, updated_at FROM worker_status WHERE worker_key = ?")
            .bind(key).fetch_one(&self.pool).await?)
    }

    pub async fn ensure_worker_status(
        &self,
        key: &str,
        label: &str,
        status: &str,
        detail: Option<&str>,
    ) -> anyhow::Result<WorkerStatus> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("INSERT OR IGNORE INTO worker_status (worker_key, label, status, detail, updated_at) VALUES (?, ?, ?, ?, ?)")
            .bind(key).bind(label).bind(status).bind(detail).bind(&now).execute(&self.pool).await?;
        Ok(sqlx::query_as::<_, WorkerStatus>("SELECT worker_key, label, status, current_run_id, detail, last_started_at, last_success_at, last_error_at, last_error, updated_at FROM worker_status WHERE worker_key = ?")
            .bind(key).fetch_one(&self.pool).await?)
    }

    pub async fn list_worker_statuses(&self) -> anyhow::Result<Vec<WorkerStatus>> {
        Ok(sqlx::query_as::<_, WorkerStatus>("SELECT worker_key, label, status, current_run_id, detail, last_started_at, last_success_at, last_error_at, last_error, updated_at FROM worker_status ORDER BY label")
            .fetch_all(&self.pool).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> Database {
        let db = Database::new("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        db
    }

    #[tokio::test]
    async fn goal_progress_and_run_steps_are_durable() {
        let db = test_db().await;
        let conversation = db.create_conversation(Some("Tracked work")).await.unwrap();
        let goal = db
            .create_goal(
                Some(conversation.id),
                "Ship progress tracking",
                "Make ongoing work visible",
                &[
                    "Persist work state".to_string(),
                    "Show it in the UI".to_string(),
                ],
            )
            .await
            .unwrap();
        assert_eq!(goal.total_tasks, 2);
        let first = &goal.tasks[0];
        db.upsert_goal_task(
            &goal.goal.id,
            Some(&first.id),
            0,
            &first.title,
            "completed",
            Some("Schema ready"),
            None,
        )
        .await
        .unwrap();
        let updated = db
            .get_goal_with_tasks(&goal.goal.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.completed_tasks, 1);
        assert!(
            db.set_goal_control_state(&goal.goal.id, "pause")
                .await
                .unwrap()
        );
        assert_eq!(
            db.get_goal(&goal.goal.id).await.unwrap().unwrap().status,
            "paused"
        );
        assert!(
            db.set_goal_control_state(&goal.goal.id, "resume")
                .await
                .unwrap()
        );
        assert_eq!(
            db.get_goal(&goal.goal.id).await.unwrap().unwrap().status,
            "active"
        );

        let run = db
            .create_work_run(NewWorkRun {
                id: Some("run-visible"),
                goal_id: Some(&goal.goal.id),
                task_id: Some(&updated.tasks[1].id),
                conversation_id: Some(conversation.id),
                kind: "chat",
                source_type: None,
                source_id: None,
                summary: "Implement the UI",
                visibility: "significant",
            })
            .await
            .unwrap();
        db.update_work_run(&run.id, "running", Some("Building the Work page"), None)
            .await
            .unwrap();
        let step = db
            .create_work_run_step(&run.id, Some("step-one"), "capability", "Build frontend")
            .await
            .unwrap();
        db.finish_work_run_step(&step.id, "completed", Some("Build passed"), None)
            .await
            .unwrap();
        db.update_work_run(&run.id, "completed", Some("Finished"), None)
            .await
            .unwrap();
        let detail = db.get_work_run_detail(&run.id).await.unwrap().unwrap();
        assert_eq!(detail.run.status, "completed");
        assert_eq!(detail.steps[0].summary.as_deref(), Some("Build passed"));
    }

    #[tokio::test]
    async fn restart_and_worker_health_are_visible_without_duplicate_history() {
        let db = test_db().await;
        let run = db
            .create_work_run(NewWorkRun {
                id: Some("run-interrupted"),
                goal_id: None,
                task_id: None,
                conversation_id: None,
                kind: "heartbeat",
                source_type: Some("heartbeat"),
                source_id: Some("check-1"),
                summary: "Continuity check",
                visibility: "quiet",
            })
            .await
            .unwrap();
        db.update_work_run(&run.id, "running", Some("Checking"), None)
            .await
            .unwrap();
        assert_eq!(db.mark_running_work_interrupted().await.unwrap(), 1);
        assert_eq!(
            db.get_work_run(&run.id).await.unwrap().unwrap().status,
            "interrupted"
        );

        db.upsert_worker_status(
            "heartbeat",
            "Heartbeat checks",
            "idle",
            None,
            Some("Ready"),
            true,
            None,
        )
        .await
        .unwrap();
        db.upsert_worker_status(
            "heartbeat",
            "Heartbeat checks",
            "degraded",
            None,
            Some("Failed"),
            false,
            Some("network error"),
        )
        .await
        .unwrap();
        let workers = db.list_worker_statuses().await.unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].status, "degraded");
        assert_eq!(workers[0].last_error.as_deref(), Some("network error"));
        assert!(
            db.list_work_runs(None, true, 10, None)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
