/// Self-initiated proactivity: unlike `process_pending_events` and
/// `process_scheduled_tasks`, this is not triggered by anything arriving or by
/// anything Jossie previously registered for itself. On its own schedule it
/// builds a synthetic event summarizing cheap-to-compute signals and lets the
/// normal event-mode triage in `generate_event_message` decide whether there
/// is a genuine reason to speak up. Most ticks should produce nothing.
async fn maybe_run_heartbeat(state: &Arc<AppState>) -> anyhow::Result<()> {
    if !state.background.heartbeat_enabled {
        return Ok(());
    }

    let last_run = state
        .db
        .get_integration_setting(HEARTBEAT_SETTINGS_NAMESPACE, HEARTBEAT_LAST_RUN_KEY)
        .await?;
    if !heartbeat_is_due(
        last_run.as_deref(),
        state.background.heartbeat_interval_secs,
        Utc::now(),
    ) {
        return Ok(());
    }

    // Claim this slot immediately (before the potentially slow LLM call below) so a
    // heartbeat run that takes a while doesn't get re-triggered by the next tick.
    state
        .db
        .set_integration_setting(
            HEARTBEAT_SETTINGS_NAMESPACE,
            HEARTBEAT_LAST_RUN_KEY,
            &Utc::now().to_rfc3339(),
        )
        .await?;

    let Some(target) = resolve_background_target(state).await? else {
        return Ok(());
    };

    if state
        .active_conversations
        .read()
        .await
        .contains(&target.conversation_id)
    {
        tracing::debug!(
            "Skipping heartbeat check; conversation {} is active",
            target.conversation_id
        );
        return Ok(());
    }

    let event = build_heartbeat_event(state, target.conversation_id).await?;
    let work_run_id = format!("heartbeat-{}", event.id);
    state
        .db
        .create_work_run(jossie_db::NewWorkRun {
            id: Some(&work_run_id),
            goal_id: None,
            task_id: None,
            conversation_id: Some(target.conversation_id),
            kind: "heartbeat",
            source_type: Some("heartbeat"),
            source_id: Some(&event.id),
            summary: "Proactive continuity check",
            visibility: "quiet",
        })
        .await?;
    state
        .db
        .update_work_run(
            &work_run_id,
            "running",
            Some("Checking whether anything needs attention"),
            None,
        )
        .await?;

    let message =
        match jossie_server::agent::generate_event_message(state, target.conversation_id, &event)
            .await
        {
            Ok(msg) => msg,
            Err(e) if e.to_string().contains("already being processed") => {
                tracing::debug!(
                    "Deferring heartbeat check - conversation {} is busy",
                    target.conversation_id
                );
                state
                    .db
                    .update_work_run(
                        &work_run_id,
                        "cancelled",
                        Some("Deferred because the conversation is busy"),
                        None,
                    )
                    .await?;
                return Ok(());
            }
            Err(e) => {
                state
                    .db
                    .annotate_work_run(
                        &work_run_id,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some("significant"),
                    )
                    .await?;
                state
                    .db
                    .update_work_run(
                        &work_run_id,
                        "failed",
                        Some("Heartbeat failed"),
                        Some(&e.to_string()),
                    )
                    .await?;
                return Err(e);
            }
        };

    tracing::info!("Heartbeat check result: {:?}", message);
    let Some(message) = message else {
        state
            .db
            .update_work_run(
                &work_run_id,
                "completed",
                Some("Nothing needed attention"),
                None,
            )
            .await?;
        return Ok(());
    };
    state
        .db
        .annotate_work_run(
            &work_run_id,
            None,
            None,
            None,
            None,
            None,
            Some("significant"),
        )
        .await?;

    maybe_send_telegram_message(state, target.telegram_chat_id, &message).await?;

    let assistant_msg = Message {
        id: Uuid::new_v4(),
        conversation_id: target.conversation_id,
        role: Role::Assistant,
        content: message,
        tool_calls: None,
        tool_call_id: None,
        name: Some("integration_event_notification".to_string()),
        attachments: None,
        response_items: None,
        created_at: Utc::now(),
    };
    persist_message(state, &assistant_msg).await?;
    state.publish_event(ServerEvent::BackgroundNotification {
        conversation_id: target.conversation_id,
        source: "heartbeat".to_string(),
        message: assistant_msg.content.clone(),
    });
    state
        .db
        .update_work_run(
            &work_run_id,
            "completed",
            Some("Proactive update surfaced"),
            None,
        )
        .await?;

    Ok(())
}

/// True when no heartbeat has run yet, or the configured interval has elapsed
/// since the last recorded run. Pulled out of `maybe_run_heartbeat` so the gating
/// logic can be unit tested without a database.
fn heartbeat_is_due(last_run_raw: Option<&str>, interval_secs: u64, now: DateTime<Utc>) -> bool {
    let Some(raw) = last_run_raw else {
        return true;
    };
    let Ok(last_run) = DateTime::parse_from_rfc3339(raw) else {
        return true;
    };
    let elapsed = now - last_run.with_timezone(&Utc);
    elapsed >= chrono::Duration::seconds(interval_secs as i64)
}

/// True when `next_run_at` falls within `[now, now + window_hours]`. Pulled out of
/// `build_heartbeat_event` so the windowing logic can be unit tested directly.
fn is_due_within_window(next_run_at: Option<&str>, window_hours: i64, now: DateTime<Utc>) -> bool {
    next_run_at
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .is_some_and(|next_run| {
            let until = next_run.with_timezone(&Utc) - now;
            until >= chrono::Duration::zero() && until <= chrono::Duration::hours(window_hours)
        })
}

async fn build_heartbeat_event(
    state: &Arc<AppState>,
    conversation_id: Uuid,
) -> anyhow::Result<IntegrationEvent> {
    let minutes_since_last_activity = state
        .db
        .list_conversations()
        .await?
        .into_iter()
        .find(|c| c.id == conversation_id)
        .map(|c| (Utc::now() - c.updated_at).num_minutes());

    let has_pending_approvals = state
        .db
        .has_blocking_pending_actions(conversation_id)
        .await?;

    let scheduled_tasks_due_soon = state
        .db
        .list_upcoming_scheduled_tasks(20)
        .await?
        .into_iter()
        .filter(|task| {
            is_due_within_window(
                task.next_run_at.as_deref(),
                HEARTBEAT_UPCOMING_WINDOW_HOURS,
                Utc::now(),
            )
        })
        .count();

    Ok(IntegrationEvent {
        id: Uuid::new_v4().to_string(),
        integration: "heartbeat".to_string(),
        account_id: "self".to_string(),
        event_type: HEARTBEAT_EVENT_TYPE.to_string(),
        dedupe_key: format!("heartbeat:{}", Uuid::new_v4()),
        payload: serde_json::json!({
            "minutes_since_last_activity": minutes_since_last_activity,
            "has_pending_approvals": has_pending_approvals,
            "scheduled_tasks_due_within_24h": scheduled_tasks_due_soon,
        }),
        status: "processing".to_string(),
        created_at: Utc::now().to_rfc3339(),
        processed_at: None,
        last_error: None,
    })
}
