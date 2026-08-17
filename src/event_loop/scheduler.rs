async fn process_scheduled_tasks(state: &Arc<AppState>) -> anyhow::Result<()> {
    let tasks = state.db.list_pending_scheduled_tasks(10).await?;

    for task in tasks {
        tracing::info!("Processing scheduled task: {}", task.id);

        // Atomically claim task to prevent duplicate execution by concurrent loop iterations.
        let claimed = state.db.mark_task_running_if_pending(&task.id).await?;
        if !claimed {
            tracing::debug!("Task {} already claimed by another worker", task.id);
            continue;
        }

        // Check if max runs exceeded
        if let Some(max) = task.max_runs
            && task.run_count >= max
        {
            state.db.mark_task_completed(&task.id).await?;
            tracing::info!("Task {} completed (max runs reached)", task.id);
            continue;
        }

        // Spawn task execution in background
        let state_clone = state.clone();
        let task_clone = task.clone();
        tokio::spawn(async move {
            if let Err(e) = execute_scheduled_task(&state_clone, &task_clone).await {
                tracing::error!("Failed to execute task {}: {}", task_clone.id, e);
                let _ = state_clone
                    .db
                    .mark_task_failed(&task_clone.id, &e.to_string())
                    .await;
            }
        });
    }

    Ok(())
}
async fn execute_scheduled_task(
    state: &Arc<AppState>,
    task: &jossie_db::ScheduledTask,
) -> anyhow::Result<()> {
    tracing::info!("Executing task {}: {}", task.id, task.task_type);

    match task.task_type.as_str() {
        "agent_run" => {
            let prompt = task
                .task_data
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let context = task
                .task_data
                .get("context")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let conversation_id: Uuid = task.conversation_id.parse()?;

            // If the conversation is currently busy (e.g. user chat), defer this run.
            if state
                .active_conversations
                .read()
                .await
                .contains(&conversation_id)
            {
                let retry_at = Utc::now() + chrono::Duration::seconds(30);
                state
                    .db
                    .update_task_next_run(&task.id, &retry_at.to_rfc3339(), false)
                    .await?;
                tracing::debug!(
                    "Deferred task {} because conversation {} is busy",
                    task.id,
                    conversation_id
                );
                return Ok(());
            }

            // Create user message for the scheduled task
            let content = if context.is_empty() {
                prompt.to_string()
            } else {
                format!("{}\n\nContext: {}", prompt, context)
            };

            let user_msg = Message {
                id: Uuid::new_v4(),
                conversation_id,
                role: Role::User,
                content,
                tool_calls: None,
                tool_call_id: None,
                name: Some("scheduled_task".to_string()),
                attachments: None,
                response_items: None,
                created_at: Utc::now(),
            };
            persist_message(state, &user_msg).await?;

            // Run the agent loop
            let response = match jossie_server::agent::run_agent_loop_with_options(
                state,
                conversation_id,
                jossie_server::agent::AgentRunOptions {
                    allow_schedule_management: false,
                    allow_oob_messages: false,
                    scheduled_execution: true,
                    authorization_context: task
                        .task_data
                        .get("authorization_context")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    goal_id: task
                        .task_data
                        .get("goal_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    task_id: task
                        .task_data
                        .get("goal_task_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    work_source_type: Some("scheduled_task".to_string()),
                    work_source_id: Some(format!("{}:{}", task.id, task.run_count + 1)),
                    work_summary: Some(prompt.to_string()),
                    resume_checkpoint_run_id: None,
                },
            )
            .await
            {
                Ok(response) => response,
                Err(e) if e.to_string().contains("already being processed") => {
                    let retry_at = Utc::now() + chrono::Duration::seconds(30);
                    state
                        .db
                        .update_task_next_run(&task.id, &retry_at.to_rfc3339(), false)
                        .await?;
                    tracing::debug!(
                        "Deferred task {} after busy-conversation race: {}",
                        task.id,
                        conversation_id
                    );
                    return Ok(());
                }
                Err(e) => return Err(e),
            };

            // Send response via Telegram if configured
            if !state.telegram.token.trim().is_empty()
                && let Some(chat) = state.db.get_latest_telegram_chat().await?
                && chat.conversation_id == conversation_id
            {
                jossie_telegram::send_message(&state.telegram.token, chat.chat_id, &response)
                    .await?;
            }
        }
        _ => {
            anyhow::bail!("Unknown task type: {}", task.task_type);
        }
    }

    // Handle scheduling based on type
    match task.schedule_type.as_str() {
        "once" => {
            state.db.mark_task_completed(&task.id).await?;
        }
        "interval" => {
            let interval_secs: i64 = task
                .schedule_value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid interval value"))?;
            let next_run = Utc::now() + chrono::Duration::seconds(interval_secs);
            state
                .db
                .update_task_next_run(&task.id, &next_run.to_rfc3339(), true)
                .await?;
        }
        "cron" => {
            let next_run = next_cron_occurrence(&task.schedule_value, Utc::now())?;
            state
                .db
                .update_task_next_run(&task.id, &next_run.to_rfc3339(), true)
                .await?;
        }
        _ => {
            anyhow::bail!("Unknown schedule type: {}", task.schedule_type);
        }
    }

    Ok(())
}

/// Computes the next fire time for a cron-scheduled task. Standard 5-field cron
/// expressions are accepted, and a 6-field form with a leading seconds field.
fn next_cron_occurrence(
    cron_expression: &str,
    after: DateTime<Utc>,
) -> anyhow::Result<DateTime<Utc>> {
    let cron = Cron::from_str(cron_expression)
        .map_err(|e| anyhow::anyhow!("Invalid cron expression '{}': {}", cron_expression, e))?;
    cron.find_next_occurrence(&after, false)
        .map_err(|e| anyhow::anyhow!("Could not compute next cron occurrence: {}", e))
}

async fn process_oob_messages(state: &Arc<AppState>) -> anyhow::Result<()> {
    let messages = state.db.list_pending_oob_messages(20).await?;
    let mut chat_cache: HashMap<Uuid, Option<i64>> = HashMap::new();

    for msg in messages {
        tracing::info!("Sending OOB message: {}", msg.id);

        let conversation_id: Uuid = msg.conversation_id.parse()?;
        let work_run_id = format!("out-of-band-{}", msg.id);
        state
            .db
            .create_work_run(jossie_db::NewWorkRun {
                id: Some(&work_run_id),
                goal_id: None,
                task_id: None,
                conversation_id: Some(conversation_id),
                kind: "delivery",
                source_type: Some("out_of_band_message"),
                source_id: Some(&msg.id),
                summary: "Deliver a queued update",
                visibility: "significant",
            })
            .await?;
        state
            .db
            .update_work_run(&work_run_id, "running", Some("Delivering an update"), None)
            .await?;

        // Resolve chat id for the specific conversation (cached for this batch).
        let chat_id = if let Some(cached) = chat_cache.get(&conversation_id) {
            *cached
        } else {
            let resolved = state
                .db
                .get_telegram_chat_for_conversation(conversation_id)
                .await?;
            chat_cache.insert(conversation_id, resolved);
            resolved
        };

        if !state.telegram.token.trim().is_empty() {
            if let Some(chat_id) = chat_id {
                match jossie_telegram::send_message(&state.telegram.token, chat_id, &msg.content)
                    .await
                {
                    Ok(_) => {
                        tracing::info!("OOB message {} sent successfully", msg.id);
                    }
                    Err(e) => {
                        state
                            .db
                            .mark_oob_message_failed(&msg.id, &e.to_string())
                            .await?;
                        state
                            .db
                            .update_work_run(
                                &work_run_id,
                                "failed",
                                Some("Delivery failed"),
                                Some(&e.to_string()),
                            )
                            .await?;
                        tracing::error!("Failed to send OOB message {}: {}", msg.id, e);
                        continue;
                    }
                }
            } else {
                tracing::debug!(
                    "No Telegram chat linked for conversation {}; delivering OOB message {} to the conversation only",
                    conversation_id,
                    msg.id
                );
            }
        }

        let assistant_msg = Message {
            id: Uuid::new_v4(),
            conversation_id,
            role: Role::Assistant,
            content: msg.content.clone(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: Some("oob_message".to_string()),
            response_items: None,
            created_at: Utc::now(),
        };
        persist_message(state, &assistant_msg).await?;
        state.publish_event(ServerEvent::BackgroundNotification {
            conversation_id,
            source: "oob_message".to_string(),
            message: assistant_msg.content.clone(),
        })
        .await;
        state.db.mark_oob_message_sent(&msg.id).await?;
        state
            .db
            .update_work_run(&work_run_id, "completed", Some("Delivered"), None)
            .await?;
    }

    Ok(())
}
