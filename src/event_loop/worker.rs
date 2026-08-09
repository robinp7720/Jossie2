#[derive(Clone, Copy, Debug)]
struct BackgroundTarget {
    conversation_id: Uuid,
    telegram_chat_id: Option<i64>,
}

pub async fn start_event_loop(state: Arc<AppState>) -> anyhow::Result<()> {
    recover_stale_processing_events(&state).await?;

    for (key, label) in [
        ("integration_events", "Integration event triage"),
        ("scheduled_tasks", "Scheduled work"),
        ("out_of_band", "Message delivery"),
        ("chat_imports", "Chat imports"),
        ("knowledge_extraction", "Knowledge extraction"),
        ("conversation_summary", "Conversation summaries"),
    ] {
        if let Ok(worker) = state
            .db
            .ensure_worker_status(key, label, "idle", Some("Ready"))
            .await
        {
            let _ = state
                .event_tx
                .send(ServerEvent::WorkerStatusUpdated { worker });
        }
    }
    update_worker(
        &state,
        "heartbeat",
        "Heartbeat checks",
        if state.heartbeat_enabled {
            "idle"
        } else {
            "disabled"
        },
        Some(if state.heartbeat_enabled {
            "Ready"
        } else {
            "Disabled in configuration"
        }),
        false,
        None,
    )
    .await;

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        BACKGROUND_WORK_INTERVAL_SECS,
    ));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let integration_poll_interval = std::time::Duration::from_secs(INTEGRATION_POLL_INTERVAL_SECS);
    let mut next_integration_poll = tokio::time::Instant::now();

    loop {
        let now = tokio::time::Instant::now();
        if now >= next_integration_poll {
            tracing::info!("Event loop iteration");
            for integration in state.registry.get_integrations() {
                let key = format!("poll:{}", integration.name());
                let label = format!("{} polling", integration.name());
                update_worker(
                    &state,
                    &key,
                    &label,
                    "running",
                    Some("Checking for updates"),
                    false,
                    None,
                )
                .await;
                match integration.poll().await {
                    Ok(()) => {
                        update_worker(
                            &state,
                            &key,
                            &label,
                            "idle",
                            Some("Last check succeeded"),
                            true,
                            None,
                        )
                        .await
                    }
                    Err(e) => {
                        tracing::error!(
                            "Poll failed for integration {}: {}",
                            integration.name(),
                            e
                        );
                        update_worker(
                            &state,
                            &key,
                            &label,
                            "degraded",
                            Some("Latest check failed"),
                            false,
                            Some(&e.to_string()),
                        )
                        .await;
                    }
                }
            }
            next_integration_poll = now + integration_poll_interval;
        }

        update_worker(
            &state,
            "integration_events",
            "Integration event triage",
            "running",
            Some("Checking queued events"),
            false,
            None,
        )
        .await;
        match process_pending_events(&state).await {
            Ok(()) => {
                update_worker(
                    &state,
                    "integration_events",
                    "Integration event triage",
                    "idle",
                    Some("Queue checked"),
                    true,
                    None,
                )
                .await
            }
            Err(e) => {
                tracing::error!("Event processing failed: {e}");
                update_worker(
                    &state,
                    "integration_events",
                    "Integration event triage",
                    "degraded",
                    Some("Processing failed"),
                    false,
                    Some(&e.to_string()),
                )
                .await;
            }
        }

        update_worker(
            &state,
            "scheduled_tasks",
            "Scheduled work",
            "running",
            Some("Checking due work"),
            false,
            None,
        )
        .await;
        match process_scheduled_tasks(&state).await {
            Ok(()) => {
                update_worker(
                    &state,
                    "scheduled_tasks",
                    "Scheduled work",
                    "idle",
                    Some("Schedule checked"),
                    true,
                    None,
                )
                .await
            }
            Err(e) => {
                tracing::error!("Scheduled task processing failed: {e}");
                update_worker(
                    &state,
                    "scheduled_tasks",
                    "Scheduled work",
                    "degraded",
                    Some("Processing failed"),
                    false,
                    Some(&e.to_string()),
                )
                .await;
            }
        }

        update_worker(
            &state,
            "out_of_band",
            "Message delivery",
            "running",
            Some("Checking queued messages"),
            false,
            None,
        )
        .await;
        match process_oob_messages(&state).await {
            Ok(()) => {
                update_worker(
                    &state,
                    "out_of_band",
                    "Message delivery",
                    "idle",
                    Some("Delivery queue checked"),
                    true,
                    None,
                )
                .await
            }
            Err(e) => {
                tracing::error!("OOB message processing failed: {e}");
                update_worker(
                    &state,
                    "out_of_band",
                    "Message delivery",
                    "degraded",
                    Some("Delivery failed"),
                    false,
                    Some(&e.to_string()),
                )
                .await;
            }
        }

        if state.heartbeat_enabled {
            update_worker(
                &state,
                "heartbeat",
                "Heartbeat checks",
                "running",
                Some("Checking whether a heartbeat is due"),
                false,
                None,
            )
            .await;
            match maybe_run_heartbeat(&state).await {
                Ok(()) => {
                    update_worker(
                        &state,
                        "heartbeat",
                        "Heartbeat checks",
                        "idle",
                        Some("Heartbeat check succeeded"),
                        true,
                        None,
                    )
                    .await
                }
                Err(e) => {
                    tracing::error!("Heartbeat check failed: {e}");
                    update_worker(
                        &state,
                        "heartbeat",
                        "Heartbeat checks",
                        "degraded",
                        Some("Heartbeat failed"),
                        false,
                        Some(&e.to_string()),
                    )
                    .await;
                }
            }
        }

        interval.tick().await;
    }
}

async fn update_worker(
    state: &Arc<AppState>,
    key: &str,
    label: &str,
    status: &str,
    detail: Option<&str>,
    success: bool,
    error: Option<&str>,
) {
    match state
        .db
        .upsert_worker_status(key, label, status, None, detail, success, error)
        .await
    {
        Ok(worker) if matches!(worker.status.as_str(), "degraded" | "disabled") => {
            let _ = state
                .event_tx
                .send(ServerEvent::WorkerStatusUpdated { worker });
        }
        Ok(_) => {}
        Err(db_error) => tracing::warn!("Failed to update worker status for {key}: {db_error}"),
    }
}

async fn recover_stale_processing_events(state: &Arc<AppState>) -> anyhow::Result<()> {
    let before =
        (Utc::now() - chrono::Duration::seconds(STALE_PROCESSING_EVENT_TIMEOUT_SECS)).to_rfc3339();
    let recovered = state
        .db
        .mark_stale_processing_integration_events_failed(
            &before,
            "Marked failed at startup because the event was left in processing by a previous worker",
        )
        .await?;

    if recovered > 0 {
        tracing::warn!("Marked {recovered} stale processing integration event(s) as failed");
    }

    Ok(())
}
