use chrono::{DateTime, Duration, Utc};
use jossie_core::integration::{Integration, ToolDefinition};
use jossie_db::Database;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub struct SchedulerIntegration {
    db: Arc<Database>,
}

impl SchedulerIntegration {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    async fn handle_schedule_task(
        &self,
        args: &str,
        conversation_id: Uuid,
    ) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Args {
            prompt: String,
            run_at: String,
        }
        let args: Args = serde_json::from_str(args)?;

        // Parse the run_at timestamp
        let run_at: DateTime<Utc> = args
            .run_at
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid timestamp format: {}", e))?;

        let task_data = serde_json::json!({
            "prompt": args.prompt,
        });

        let task_id = self
            .db
            .create_scheduled_task(
                conversation_id,
                "agent_run",
                &task_data,
                "once",
                &args.run_at,
                Some(&run_at.to_rfc3339()),
                Some(1),
            )
            .await?;

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
        #[derive(Deserialize)]
        struct Args {
            prompt: String,
            interval_seconds: i64,
            #[serde(default)]
            max_runs: Option<i64>,
        }
        let args: Args = serde_json::from_str(args)?;

        if args.interval_seconds < 60 {
            anyhow::bail!("Interval must be at least 60 seconds");
        }

        let task_data = serde_json::json!({
            "prompt": args.prompt,
        });

        // Calculate first run time
        let first_run = Utc::now() + Duration::seconds(args.interval_seconds);

        let task_id = self
            .db
            .create_scheduled_task(
                conversation_id,
                "agent_run",
                &task_data,
                "interval",
                &args.interval_seconds.to_string(),
                Some(&first_run.to_rfc3339()),
                args.max_runs,
            )
            .await?;

        let max_info = args
            .max_runs
            .map(|n| format!(" (max {} runs)", n))
            .unwrap_or_else(|| " (indefinitely)".to_string());
        Ok(format!(
            "Scheduled recurring task {} every {} seconds{}",
            task_id, args.interval_seconds, max_info
        ))
    }

    async fn handle_cancel_task(&self, args: &str) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Args {
            task_id: String,
        }
        let args: Args = serde_json::from_str(args)?;

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
        #[derive(Deserialize)]
        struct Args {
            message: String,
            #[serde(default = "default_priority")]
            priority: String,
        }

        fn default_priority() -> String {
            "normal".to_string()
        }

        let args: Args = serde_json::from_str(args)?;

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
            ToolDefinition {
                name: "schedule_task".to_string(),
                description: "Schedule a one-time task for the agent to run at a specific time. Optional context can be added to the prompt if needed.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": "The prompt/task for the agent to execute when the task runs. You can include any necessary context directly in the prompt."
                        },
                        "run_at": {
                            "type": "string",
                            "description": "ISO 8601 timestamp when to run the task (e.g., '2026-02-01T12:00:00Z')"
                        }
                    },
                    "required": ["prompt", "run_at"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "schedule_recurring_task".to_string(),
                description: "Schedule a recurring task that runs at regular intervals. The task will run indefinitely unless it fails. To limit runs, mention it in the prompt (e.g., 'check emails 3 times').".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": "The prompt/task for the agent to execute on each run. Include any run limits or context directly in the prompt."
                        },
                        "interval_seconds": {
                            "type": "integer",
                            "description": "Interval in seconds between runs (minimum 60)"
                        }
                    },
                    "required": ["prompt", "interval_seconds"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "cancel_scheduled_task".to_string(),
                description: "Cancel a previously scheduled task".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "The ID of the task to cancel"
                        }
                    },
                    "required": ["task_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "list_scheduled_tasks".to_string(),
                description: "List all active scheduled tasks for this conversation".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "send_user_message".to_string(),
                description: "Send a message to the user outside of the normal chat flow (out-of-band notification). Default priority is 'normal'.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "The message content to send to the user"
                        }
                    },
                    "required": ["message"],
                    "additionalProperties": false
                }),
            },
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
            "cancel_scheduled_task" => self.handle_cancel_task(arguments).await,
            "list_scheduled_tasks" => self.handle_list_tasks(conv_id).await,
            "send_user_message" => self.handle_send_message(arguments, conv_id).await,
            _ => anyhow::bail!("Unknown tool: {tool_name}"),
        }
    }
}
