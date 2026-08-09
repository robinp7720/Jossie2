async fn run_goal_notification_loop(
    bot: Bot,
    state: Arc<AppState>,
    runtime: Arc<TelegramRuntime>,
    mut events: tokio::sync::broadcast::Receiver<ServerEvent>,
) {
    if let Err(error) = reconcile_goal_notifications(&bot, &state, &runtime).await {
        tracing::warn!("Could not reconcile Telegram goal notifications: {error}");
    }
    loop {
        match events.recv().await {
            Ok(ServerEvent::GoalUpdated {
                conversation_id,
                goal,
            }) => {
                if let Err(error) =
                    deliver_goal_notification(&bot, &state, &runtime, conversation_id, &goal, true)
                        .await
                {
                    tracing::warn!(
                        goal_id = %goal.goal.id,
                        "Could not deliver Telegram goal update: {error}"
                    );
                }
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    skipped,
                    "Telegram goal notifier lagged; reconciling durable state"
                );
                if let Err(error) = reconcile_goal_notifications(&bot, &state, &runtime).await {
                    tracing::warn!("Could not reconcile Telegram goal notifications: {error}");
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn reconcile_goal_notifications(
    bot: &Bot,
    state: &AppState,
    runtime: &TelegramRuntime,
) -> anyhow::Result<()> {
    for goal in state.db.list_goals(true).await? {
        let Some(conversation_id) = goal
            .goal
            .conversation_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok())
        else {
            continue;
        };
        deliver_goal_notification(bot, state, runtime, conversation_id, &goal, false).await?;
    }
    Ok(())
}

async fn deliver_goal_notification(
    bot: &Bot,
    state: &AppState,
    runtime: &TelegramRuntime,
    conversation_id: Uuid,
    goal: &jossie_db::GoalWithTasks,
    include_completed: bool,
) -> anyhow::Result<()> {
    if !goal_needs_proactive_notification(goal, include_completed) {
        return Ok(());
    }
    let fingerprint = goal_notification_fingerprint(goal);
    if state
        .db
        .telegram_goal_notification_fingerprint(&goal.goal.id)
        .await?
        .as_deref()
        == Some(fingerprint.as_str())
    {
        return Ok(());
    }
    let Some(chat_id) = state
        .db
        .get_telegram_chat_for_conversation(conversation_id)
        .await?
    else {
        return Ok(());
    };
    if chat_is_active(runtime, chat_id).await {
        state
            .db
            .mark_telegram_goal_notification(&goal.goal.id, &fingerprint)
            .await?;
        return Ok(());
    }
    send_reply(
        bot,
        ChatId(chat_id),
        None,
        &proactive_goal_notification(goal),
        None,
    )
    .await?;
    state
        .db
        .mark_telegram_goal_notification(&goal.goal.id, &fingerprint)
        .await?;
    Ok(())
}

fn goal_needs_proactive_notification(
    goal: &jossie_db::GoalWithTasks,
    include_completed: bool,
) -> bool {
    if goal.goal.status == "cancelled" {
        return false;
    }
    if goal.goal.status == "completed" {
        return include_completed;
    }
    matches!(goal.goal.status.as_str(), "blocked" | "paused")
        || goal.tasks.iter().any(|task| task.status == "blocked")
}

fn goal_notification_fingerprint(goal: &jossie_db::GoalWithTasks) -> String {
    serde_json::json!({
        "status": goal.goal.status,
        "blocker": goal.goal.blocker,
        "completed": goal.completed_tasks,
        "total": goal.total_tasks,
        "tasks": goal.tasks.iter().map(|task| serde_json::json!({
            "id": task.id,
            "status": task.status,
            "blocker": task.blocker,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

fn proactive_goal_notification(goal: &jossie_db::GoalWithTasks) -> String {
    let title = &goal.goal.title;
    if let Some(blocker) = goal_blocker(goal) {
        let progress = if goal.total_tasks == 0 {
            String::new()
        } else {
            format!(
                " I've finished {} of {} parts, but I can't finish the next part yet.",
                goal.completed_tasks, goal.total_tasks
            )
        };
        return format!(
            "A quick update on “{title}”.{progress} I'm missing: {} Send that here when you have it and I'll continue.",
            jossie_server::events::preview_text(blocker, 480)
        );
    }
    match goal.goal.status.as_str() {
        "paused" => format!(
            "A quick update on “{title}”: I've saved my place. Just say “continue” when you want me to pick it back up."
        ),
        "completed" => format!("A quick update: I've finished “{title}”."),
        _ => conversational_goal_status(Some(goal)),
    }
}

fn goal_blocker(goal: &jossie_db::GoalWithTasks) -> Option<&str> {
    goal.goal.blocker.as_deref().or_else(|| {
        goal.tasks
            .iter()
            .find(|task| task.status == "blocked")
            .and_then(|task| task.blocker.as_deref().or(Some(task.title.as_str())))
    })
}

fn is_polling_conflict(error: &RequestError) -> bool {
    matches!(
        error,
        RequestError::Api(ApiError::TerminatedByOtherGetUpdates)
    )
}

