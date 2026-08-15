use chrono::{DateTime, Duration, Utc};
use croner::Cron;
use jossie_core::integration::{Integration, ToolDefinition};
use jossie_db::Database;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

pub struct SchedulerIntegration {
    db: Arc<Database>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScheduleTaskArgs {
    prompt: String,
    run_at: String,
    #[serde(default, rename = "__authorization_context")]
    #[schemars(skip)]
    authorization_context: String,
    #[serde(default, rename = "__goal_id")]
    #[schemars(skip)]
    goal_id: Option<String>,
    #[serde(default, rename = "__goal_task_id")]
    #[schemars(skip)]
    goal_task_id: Option<String>,
    #[serde(default, rename = "__conversation_id")]
    #[schemars(skip)]
    _conversation_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScheduleRecurringArgs {
    prompt: String,
    interval_seconds: i64,
    #[serde(default)]
    max_runs: Option<i64>,
    #[serde(default, rename = "__authorization_context")]
    #[schemars(skip)]
    authorization_context: String,
    #[serde(default, rename = "__goal_id")]
    #[schemars(skip)]
    goal_id: Option<String>,
    #[serde(default, rename = "__goal_task_id")]
    #[schemars(skip)]
    goal_task_id: Option<String>,
    #[serde(default, rename = "__conversation_id")]
    #[schemars(skip)]
    _conversation_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScheduleCronArgs {
    prompt: String,
    cron_expression: String,
    #[serde(default)]
    max_runs: Option<i64>,
    #[serde(default, rename = "__authorization_context")]
    #[schemars(skip)]
    authorization_context: String,
    #[serde(default, rename = "__goal_id")]
    #[schemars(skip)]
    goal_id: Option<String>,
    #[serde(default, rename = "__goal_task_id")]
    #[schemars(skip)]
    goal_task_id: Option<String>,
    #[serde(default, rename = "__conversation_id")]
    #[schemars(skip)]
    _conversation_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CancelScheduledTaskArgs {
    task_id: String,
    #[serde(default, rename = "__conversation_id")]
    #[schemars(skip)]
    _conversation_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListScheduledTasksArgs {
    #[serde(default, rename = "__conversation_id")]
    #[schemars(skip)]
    _conversation_id: Option<String>,
}

fn default_message_priority() -> String {
    "normal".to_string()
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendUserMessageArgs {
    message: String,
    #[serde(default = "default_message_priority")]
    priority: String,
    #[serde(default, rename = "__conversation_id")]
    #[schemars(skip)]
    _conversation_id: Option<String>,
}

impl SchedulerIntegration {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    fn normalize_prompt(prompt: &str) -> String {
        prompt.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    async fn find_existing_agent_run_task(
        &self,
        conversation_id: Uuid,
        schedule_type: &str,
        schedule_value: &str,
        prompt: &str,
    ) -> anyhow::Result<Option<String>> {
        let prompt_norm = Self::normalize_prompt(prompt);
        let tasks = self
            .db
            .list_scheduled_tasks_for_conversation(conversation_id)
            .await?;

        for task in tasks {
            if task.task_type != "agent_run"
                || task.schedule_type != schedule_type
                || task.schedule_value != schedule_value
            {
                continue;
            }

            let existing_prompt = task
                .task_data
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if Self::normalize_prompt(existing_prompt) == prompt_norm {
                return Ok(Some(task.id));
            }
        }

        Ok(None)
    }

    async fn handle_schedule_task(
        &self,
        args: &str,
        conversation_id: Uuid,
    ) -> anyhow::Result<String> {
        let args: ScheduleTaskArgs = serde_json::from_str(args)?;

        // Parse the run_at timestamp
        let run_at: DateTime<Utc> = args
            .run_at
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid timestamp format: {}", e))?;
        let now = Utc::now();
        if run_at <= now {
            anyhow::bail!("run_at must be in the future");
        }
        let run_at_rfc3339 = run_at.to_rfc3339();

        if let Some(existing_id) = self
            .find_existing_agent_run_task(conversation_id, "once", &run_at_rfc3339, &args.prompt)
            .await?
        {
            return Ok(format!(
                "A matching one-time task already exists: {}",
                existing_id
            ));
        }

        let task_data = serde_json::json!({
            "prompt": args.prompt,
            "authorization_context": args.authorization_context,
            "goal_id": args.goal_id,
            "goal_task_id": args.goal_task_id,
        });

        let task_id = self
            .db
            .create_scheduled_task(
                conversation_id,
                "agent_run",
                &task_data,
                "once",
                &run_at_rfc3339,
                Some(&run_at_rfc3339),
                Some(1),
            )
            .await?;
        if let Some(goal_task_id) = args.goal_task_id.as_deref() {
            self.db
                .link_goal_task_source(goal_task_id, "scheduled_task", &task_id)
                .await?;
        }

        Ok(format!(
            "Scheduled task {} to run at {}",
            task_id,
            run_at.format("%Y-%m-%d %H:%M:%S UTC")
        ))
    }

    async fn handle_schedule_recurring_task(
        &self,
        args: &str,
        conversation_id: Uuid,
    ) -> anyhow::Result<String> {
        let args: ScheduleRecurringArgs = serde_json::from_str(args)?;

        if args.interval_seconds < 60 {
            anyhow::bail!("Interval must be at least 60 seconds");
        }

        let task_data = serde_json::json!({
            "prompt": args.prompt,
            "authorization_context": args.authorization_context,
            "goal_id": args.goal_id,
            "goal_task_id": args.goal_task_id,
        });

        // Calculate first run time
        let first_run = Utc::now() + Duration::seconds(args.interval_seconds);
        let interval_value = args.interval_seconds.to_string();

        if let Some(existing_id) = self
            .find_existing_agent_run_task(
                conversation_id,
                "interval",
                &interval_value,
                &args.prompt,
            )
            .await?
        {
            return Ok(format!(
                "A matching recurring task already exists: {}",
                existing_id
            ));
        }

        let task_id = self
            .db
            .create_scheduled_task(
                conversation_id,
                "agent_run",
                &task_data,
                "interval",
                &interval_value,
                Some(&first_run.to_rfc3339()),
                args.max_runs,
            )
            .await?;
        if let Some(goal_task_id) = args.goal_task_id.as_deref() {
            self.db
                .link_goal_task_source(goal_task_id, "scheduled_task", &task_id)
                .await?;
        }

        let max_info = args
            .max_runs
            .map(|n| format!(" (max {} runs)", n))
            .unwrap_or_else(|| " (indefinitely)".to_string());
        Ok(format!(
            "Scheduled recurring task {} every {} seconds{}",
            task_id, args.interval_seconds, max_info
        ))
    }

    async fn handle_schedule_cron_task(
        &self,
        args: &str,
        conversation_id: Uuid,
    ) -> anyhow::Result<String> {
        let args: ScheduleCronArgs = serde_json::from_str(args)?;

        let cron_expression = args.cron_expression.trim().to_string();
        let cron = Cron::from_str(&cron_expression)
            .map_err(|e| anyhow::anyhow!("Invalid cron expression '{}': {}", cron_expression, e))?;
        let next_run = cron
            .find_next_occurrence(&Utc::now(), false)
            .map_err(|e| anyhow::anyhow!("Could not compute the next run time: {}", e))?;

        let task_data = serde_json::json!({
            "prompt": args.prompt,
            "authorization_context": args.authorization_context,
            "goal_id": args.goal_id,
            "goal_task_id": args.goal_task_id,
        });

        if let Some(existing_id) = self
            .find_existing_agent_run_task(conversation_id, "cron", &cron_expression, &args.prompt)
            .await?
        {
            return Ok(format!(
                "A matching cron task already exists: {}",
                existing_id
            ));
        }

        let task_id = self
            .db
            .create_scheduled_task(
                conversation_id,
                "agent_run",
                &task_data,
                "cron",
                &cron_expression,
                Some(&next_run.to_rfc3339()),
                args.max_runs,
            )
            .await?;
        if let Some(goal_task_id) = args.goal_task_id.as_deref() {
            self.db
                .link_goal_task_source(goal_task_id, "scheduled_task", &task_id)
                .await?;
        }

        let max_info = args
            .max_runs
            .map(|n| format!(" (max {} runs)", n))
            .unwrap_or_else(|| " (indefinitely)".to_string());
        Ok(format!(
            "Scheduled cron task {} ('{}'), next run {}{}",
            task_id,
            cron_expression,
            next_run.format("%Y-%m-%d %H:%M:%S UTC"),
            max_info
        ))
    }

    async fn handle_cancel_task(&self, args: &str) -> anyhow::Result<String> {
        let args: CancelScheduledTaskArgs = serde_json::from_str(args)?;

        self.db.cancel_scheduled_task(&args.task_id).await?;
        Ok(format!("Cancelled task {}", args.task_id))
    }

    async fn handle_list_tasks(&self, conversation_id: Uuid) -> anyhow::Result<String> {
        let tasks = self
            .db
            .list_scheduled_tasks_for_conversation(conversation_id)
            .await?;

        if tasks.is_empty() {
            return Ok("No scheduled tasks found for this conversation".to_string());
        }

        #[derive(Serialize)]
        struct TaskSummary {
            id: String,
            task_type: String,
            schedule_type: String,
            status: String,
            next_run_at: Option<String>,
            run_count: i64,
        }

        let summaries: Vec<TaskSummary> = tasks
            .into_iter()
            .map(|t| TaskSummary {
                id: t.id,
                task_type: t.task_type,
                schedule_type: t.schedule_type,
                status: t.status,
                next_run_at: t.next_run_at,
                run_count: t.run_count,
            })
            .collect();

        Ok(serde_json::to_string_pretty(&summaries)?)
    }

    async fn handle_send_message(
        &self,
        args: &str,
        conversation_id: Uuid,
    ) -> anyhow::Result<String> {
        let args: SendUserMessageArgs = serde_json::from_str(args)?;

        // Validate priority
        if !["low", "normal", "high", "urgent"].contains(&args.priority.as_str()) {
            anyhow::bail!("Priority must be one of: low, normal, high, urgent");
        }

        let msg_id = self
            .db
            .queue_oob_message(conversation_id, &args.message, &args.priority)
            .await?;

        Ok(format!(
            "Queued message {} with {} priority",
            msg_id, args.priority
        ))
    }
}

#[async_trait::async_trait]
impl Integration for SchedulerIntegration {
    fn name(&self) -> &str {
        "scheduler"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::for_args::<ScheduleTaskArgs>(
                "schedule_task",
                "Schedule a one-time task for the agent to run at a specific time. Optional context can be added to the prompt if needed.",
            ),
            ToolDefinition::for_args::<ScheduleRecurringArgs>(
                "schedule_recurring_task",
                "Schedule a recurring task that runs at regular intervals. Use max_runs to limit how many executions happen.",
            ),
            ToolDefinition::for_args::<ScheduleCronArgs>(
                "schedule_cron_task",
                "Schedule a recurring task on a cron expression (e.g. weekday mornings, the first of the month) instead of a fixed interval. Use this when the cadence follows a calendar pattern rather than a simple repeat interval; use schedule_recurring_task for plain fixed intervals.",
            ),
            ToolDefinition::for_args::<CancelScheduledTaskArgs>(
                "cancel_scheduled_task",
                "Cancel a previously scheduled task",
            ),
            ToolDefinition::for_args::<ListScheduledTasksArgs>(
                "list_scheduled_tasks",
                "List all active scheduled tasks for this conversation",
            ),
            ToolDefinition::for_args::<SendUserMessageArgs>(
                "send_user_message",
                "Send a message to the user outside of the normal chat flow (out-of-band notification). Default priority is 'normal'.",
            ),
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        // When executed during agent loop, we don't have direct access to conversation_id
        // We'll need to pass it somehow. For now, let's extract it from the arguments
        // This is a limitation we'll address in the agent loop integration

        // Try to extract conversation_id from a special field
        #[derive(Deserialize)]
        struct WithConvId {
            #[serde(rename = "__conversation_id")]
            conversation_id: Option<String>,
        }

        let conv_id = serde_json::from_str::<WithConvId>(arguments)
            .ok()
            .and_then(|w| w.conversation_id)
            .and_then(|s| s.parse::<Uuid>().ok())
            .unwrap_or(Uuid::nil());

        match tool_name {
            "schedule_task" => self.handle_schedule_task(arguments, conv_id).await,
            "schedule_recurring_task" => {
                self.handle_schedule_recurring_task(arguments, conv_id)
                    .await
            }
            "schedule_cron_task" => self.handle_schedule_cron_task(arguments, conv_id).await,
            "cancel_scheduled_task" => self.handle_cancel_task(arguments).await,
            "list_scheduled_tasks" => self.handle_list_tasks(conv_id).await,
            "send_user_message" => self.handle_send_message(arguments, conv_id).await,
            _ => anyhow::bail!("Unknown tool: {tool_name}"),
        }
    }
}
