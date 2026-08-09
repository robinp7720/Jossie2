async fn process_pending_events(state: &Arc<AppState>) -> anyhow::Result<()> {
    let Some(target) = resolve_background_target(state).await? else {
        return Ok(());
    };

    let events = state
        .db
        .list_pending_integration_events(PENDING_LIMIT)
        .await?;
    let mut email_events = Vec::new();
    let mut calendar_events = Vec::new();
    for event in events {
        if is_email_event(&event) {
            email_events.push(event);
            continue;
        }
        if is_calendar_event(&event) {
            calendar_events.push(event);
            continue;
        }

        tracing::info!("Processing event: {}", event.id);
        if let Err(e) = handle_event(state, &target, &event).await {
            tracing::error!("Event processing failed for {}: {}", event.id, e);
            state
                .db
                .mark_integration_event_failed(&event.id, &e.to_string())
                .await?;
        }
    }

    if !email_events.is_empty() {
        tracing::info!("Batch processing {} email events", email_events.len());
        if let Err(e) = handle_email_event_batch(state, &target, &email_events).await {
            tracing::error!("Email batch processing failed: {}", e);
        }
    }
    if !calendar_events.is_empty() {
        tracing::info!("Batch processing {} calendar events", calendar_events.len());
        if let Err(e) = handle_calendar_event_batch(state, &target, &calendar_events).await {
            tracing::error!("Calendar batch processing failed: {}", e);
        }
    }

    Ok(())
}

async fn resolve_background_target(
    state: &Arc<AppState>,
) -> anyhow::Result<Option<BackgroundTarget>> {
    if let Some(chat) = state.db.get_latest_telegram_chat().await? {
        return Ok(Some(BackgroundTarget {
            conversation_id: chat.conversation_id,
            telegram_chat_id: Some(chat.chat_id),
        }));
    }

    let Some(conversation_id) = state.db.get_latest_conversation_id().await? else {
        tracing::debug!("Skipping background delivery: no conversations exist yet");
        return Ok(None);
    };

    let telegram_chat_id = state
        .db
        .get_telegram_chat_for_conversation(conversation_id)
        .await?;

    Ok(Some(BackgroundTarget {
        conversation_id,
        telegram_chat_id,
    }))
}

async fn maybe_send_telegram_message(
    state: &Arc<AppState>,
    telegram_chat_id: Option<i64>,
    message: &str,
) -> anyhow::Result<()> {
    if state.telegram_token.trim().is_empty() {
        return Ok(());
    }

    let Some(chat_id) = telegram_chat_id else {
        return Ok(());
    };

    jossie_telegram::send_message(&state.telegram_token, chat_id, message).await
}

fn is_email_event(event: &IntegrationEvent) -> bool {
    matches!(event.event_type.as_str(), "new_email" | "gmail_new_message")
}

fn is_calendar_event(event: &IntegrationEvent) -> bool {
    matches!(event.event_type.as_str(), "calendar_event_updated")
}

async fn handle_event(
    state: &Arc<AppState>,
    target: &BackgroundTarget,
    event: &IntegrationEvent,
) -> anyhow::Result<()> {
    tracing::info!("Processing event: {}", event.id);

    // Atomically mark event as processing to prevent concurrent processing
    let claimed = state
        .db
        .mark_integration_event_processing(&event.id)
        .await?;
    if !claimed {
        tracing::debug!("Event {} already being processed, skipping", event.id);
        return Ok(());
    }

    let work_run_id = format!("integration-event-{}", event.id);
    state
        .db
        .create_work_run(jossie_db::NewWorkRun {
            id: Some(&work_run_id),
            goal_id: None,
            task_id: None,
            conversation_id: Some(target.conversation_id),
            kind: "integration_event",
            source_type: Some("integration_event"),
            source_id: Some(&event.id),
            summary: "Review an incoming integration event",
            visibility: "quiet",
        })
        .await?;
    state
        .db
        .update_work_run(
            &work_run_id,
            "running",
            Some("Reviewing whether this needs attention"),
            None,
        )
        .await?;

    // If we fail anywhere below, reset the event status to 'new' so it can be retried
    let result = process_event_inner(state, target, event).await;

    match result {
        Ok(surfaced) => {
            state
                .db
                .annotate_work_run(
                    &work_run_id,
                    None,
                    None,
                    None,
                    None,
                    None,
                    surfaced.then_some("significant"),
                )
                .await?;
            state
                .db
                .update_work_run(
                    &work_run_id,
                    "completed",
                    Some(if surfaced {
                        "Update surfaced"
                    } else {
                        "No action needed"
                    }),
                    None,
                )
                .await?;
            Ok(())
        }
        Err(e) => {
            // Reset event to 'new' status on failure so it can be retried
            state
                .db
                .mark_integration_event_failed(&event.id, &e.to_string())
                .await?;
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
                    Some("Event review failed"),
                    Some(&e.to_string()),
                )
                .await?;
            Err(e)
        }
    }
}

async fn process_event_inner(
    state: &Arc<AppState>,
    target: &BackgroundTarget,
    event: &IntegrationEvent,
) -> anyhow::Result<bool> {
    // NEW: Extract entities from event and enrich with graph context
    let entities = extract_event_entities(event);
    for entity in &entities {
        if let Ok(nodes) = state.db.graph_find_nodes(entity).await {
            if !nodes.is_empty() {
                tracing::info!("Enriching event with graph context for: {}", entity);
                // Graph context will be automatically injected in generate_event_message
                // via the normal context building mechanism
            }
        }
    }

    let message =
        match jossie_server::agent::generate_event_message(state, target.conversation_id, event)
            .await
        {
            Ok(msg) => msg,
            Err(e) if e.to_string().contains("already being processed") => {
                // Conversation is busy with Telegram chat or another event, retry later.
                tracing::debug!(
                    "Deferring event {} - conversation {} is busy",
                    event.id,
                    target.conversation_id
                );
                state.db.mark_integration_event_new(&event.id).await?;
                return Ok(false);
            }
            Err(e) => return Err(e),
        };

    tracing::info!("Generated message: {:?}", message);
    let Some(message) = message else {
        state.db.mark_integration_event_processed(&event.id).await?;
        return Ok(false);
    };

    if target.telegram_chat_id.is_some() && !state.telegram_token.trim().is_empty() {
        tracing::info!("Sending message: {}", message);
    }
    maybe_send_telegram_message(state, target.telegram_chat_id, &message).await?;
    if target.telegram_chat_id.is_some() && !state.telegram_token.trim().is_empty() {
        tracing::info!("Message sent: {}", message);
    }

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
        source: "integration_event".to_string(),
        message: assistant_msg.content.clone(),
    });
    state.db.mark_integration_event_processed(&event.id).await?;
    Ok(true)
}

async fn handle_email_event_batch(
    state: &Arc<AppState>,
    target: &BackgroundTarget,
    events: &[IntegrationEvent],
) -> anyhow::Result<()> {
    let mut claimed_events = Vec::new();
    for event in events {
        let claimed = state
            .db
            .mark_integration_event_processing(&event.id)
            .await?;
        if claimed {
            claimed_events.push(event.clone());
        } else {
            tracing::debug!("Event {} already being processed, skipping", event.id);
        }
    }

    if claimed_events.is_empty() {
        return Ok(());
    }

    let result = process_email_event_batch_inner(state, target, &claimed_events).await;
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            for event in &claimed_events {
                state
                    .db
                    .mark_integration_event_failed(&event.id, &e.to_string())
                    .await?;
            }
            Err(e)
        }
    }
}

async fn handle_calendar_event_batch(
    state: &Arc<AppState>,
    target: &BackgroundTarget,
    events: &[IntegrationEvent],
) -> anyhow::Result<()> {
    let mut claimed_events = Vec::new();
    for event in events {
        let claimed = state
            .db
            .mark_integration_event_processing(&event.id)
            .await?;
        if claimed {
            claimed_events.push(event.clone());
        } else {
            tracing::debug!("Event {} already being processed, skipping", event.id);
        }
    }

    if claimed_events.is_empty() {
        return Ok(());
    }

    let result = process_calendar_event_batch_inner(state, target, &claimed_events).await;
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            for event in &claimed_events {
                state
                    .db
                    .mark_integration_event_failed(&event.id, &e.to_string())
                    .await?;
            }
            Err(e)
        }
    }
}

async fn process_email_event_batch_inner(
    state: &Arc<AppState>,
    target: &BackgroundTarget,
    events: &[IntegrationEvent],
) -> anyhow::Result<()> {
    // Enrich each event's entities before generating a combined message.
    for event in events {
        let entities = extract_event_entities(event);
        for entity in &entities {
            if let Ok(nodes) = state.db.graph_find_nodes(entity).await {
                if !nodes.is_empty() {
                    tracing::info!("Enriching event with graph context for: {}", entity);
                }
            }
        }
    }

    let batched_event = build_email_batch_event(events);
    let message = match jossie_server::agent::generate_event_message(
        state,
        target.conversation_id,
        &batched_event,
    )
    .await
    {
        Ok(msg) => msg,
        Err(e) if e.to_string().contains("already being processed") => {
            tracing::debug!(
                "Deferring email batch - conversation {} is busy",
                target.conversation_id
            );
            mark_events_new(state, events).await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    tracing::info!("Generated batched email message: {:?}", message);
    let Some(message) = message else {
        mark_events_processed(state, events).await?;
        return Ok(());
    };

    if target.telegram_chat_id.is_some() && !state.telegram_token.trim().is_empty() {
        tracing::info!("Sending batched email message: {}", message);
    }
    maybe_send_telegram_message(state, target.telegram_chat_id, &message).await?;
    if target.telegram_chat_id.is_some() && !state.telegram_token.trim().is_empty() {
        tracing::info!("Batched email message sent: {}", message);
    }

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
        source: "email_batch".to_string(),
        message: assistant_msg.content.clone(),
    });
    mark_events_processed(state, events).await?;

    Ok(())
}

async fn process_calendar_event_batch_inner(
    state: &Arc<AppState>,
    target: &BackgroundTarget,
    events: &[IntegrationEvent],
) -> anyhow::Result<()> {
    let (reduced_events, omitted_count) = reduce_calendar_events(events, CALENDAR_BATCH_MAX_EVENTS);
    if reduced_events.is_empty() {
        tracing::info!(
            "Skipping calendar batch message; all {} events were filtered",
            events.len()
        );
        mark_events_processed(state, events).await?;
        return Ok(());
    }

    // Enrich each event's entities before generating a combined message.
    for event in &reduced_events {
        let entities = extract_event_entities(event);
        for entity in &entities {
            if let Ok(nodes) = state.db.graph_find_nodes(entity).await {
                if !nodes.is_empty() {
                    tracing::info!(
                        "Enriching calendar event with graph context for: {}",
                        entity
                    );
                }
            }
        }
    }

    let batched_event = build_calendar_batch_event(&reduced_events, events.len(), omitted_count);
    let message = match jossie_server::agent::generate_event_message(
        state,
        target.conversation_id,
        &batched_event,
    )
    .await
    {
        Ok(msg) => msg,
        Err(e) if e.to_string().contains("already being processed") => {
            tracing::debug!(
                "Deferring calendar batch - conversation {} is busy",
                target.conversation_id
            );
            mark_events_new(state, events).await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    tracing::info!("Generated batched calendar message: {:?}", message);
    let Some(message) = message else {
        mark_events_processed(state, events).await?;
        return Ok(());
    };

    if target.telegram_chat_id.is_some() && !state.telegram_token.trim().is_empty() {
        tracing::info!("Sending batched calendar message: {}", message);
    }
    maybe_send_telegram_message(state, target.telegram_chat_id, &message).await?;
    if target.telegram_chat_id.is_some() && !state.telegram_token.trim().is_empty() {
        tracing::info!("Batched calendar message sent: {}", message);
    }

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
        source: "calendar_batch".to_string(),
        message: assistant_msg.content.clone(),
    });
    mark_events_processed(state, events).await?;

    Ok(())
}

fn build_email_batch_event(events: &[IntegrationEvent]) -> IntegrationEvent {
    let mut sorted_events: Vec<IntegrationEvent> = events.to_vec();
    sorted_events.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    let integration = if sorted_events
        .iter()
        .all(|event| event.integration == sorted_events[0].integration)
    {
        sorted_events[0].integration.clone()
    } else {
        "mixed".to_string()
    };

    let account_id = if sorted_events
        .iter()
        .all(|event| event.account_id == sorted_events[0].account_id)
    {
        sorted_events[0].account_id.clone()
    } else {
        "mixed".to_string()
    };

    let email_events: Vec<serde_json::Value> = sorted_events
        .iter()
        .map(|event| {
            serde_json::json!({
                "id": event.id,
                "integration": event.integration,
                "account_id": event.account_id,
                "event_type": event.event_type,
                "created_at": event.created_at,
                "payload": event.payload,
            })
        })
        .collect();

    IntegrationEvent {
        id: Uuid::new_v4().to_string(),
        integration,
        account_id,
        event_type: "new_email_batch".to_string(),
        dedupe_key: format!("batch:{}:{}", sorted_events.len(), Uuid::new_v4()),
        payload: serde_json::json!({
            "count": sorted_events.len(),
            "emails": email_events,
        }),
        status: "processing".to_string(),
        created_at: Utc::now().to_rfc3339(),
        processed_at: None,
        last_error: None,
    }
}

fn build_calendar_batch_event(
    events: &[IntegrationEvent],
    original_count: usize,
    omitted_count: usize,
) -> IntegrationEvent {
    let integration = if events
        .iter()
        .all(|event| event.integration == events[0].integration)
    {
        events[0].integration.clone()
    } else {
        "mixed".to_string()
    };

    let account_id = if events
        .iter()
        .all(|event| event.account_id == events[0].account_id)
    {
        events[0].account_id.clone()
    } else {
        "mixed".to_string()
    };

    let calendar_events: Vec<serde_json::Value> = events
        .iter()
        .map(|event| {
            serde_json::json!({
                "id": event.id,
                "integration": event.integration,
                "account_id": event.account_id,
                "event_type": event.event_type,
                "created_at": event.created_at,
                "payload": event.payload,
            })
        })
        .collect();

    IntegrationEvent {
        id: Uuid::new_v4().to_string(),
        integration,
        account_id,
        event_type: "calendar_event_batch".to_string(),
        dedupe_key: format!("calendar_batch:{}:{}", original_count, Uuid::new_v4()),
        payload: serde_json::json!({
            "count": events.len(),
            "original_count": original_count,
            "omitted_count": omitted_count,
            "events": calendar_events,
        }),
        status: "processing".to_string(),
        created_at: Utc::now().to_rfc3339(),
        processed_at: None,
        last_error: None,
    }
}

fn reduce_calendar_events(
    events: &[IntegrationEvent],
    max_events: usize,
) -> (Vec<IntegrationEvent>, usize) {
    if events.is_empty() {
        return (Vec::new(), 0);
    }

    let mut best_by_key: HashMap<String, IntegrationEvent> = HashMap::new();
    for event in events {
        if is_low_value_calendar_event(event) {
            continue;
        }

        let key = calendar_logical_key(event);
        match best_by_key.get(&key) {
            Some(existing) if calendar_updated_value(event) <= calendar_updated_value(existing) => {
            }
            _ => {
                best_by_key.insert(key, event.clone());
            }
        }
    }

    let mut reduced: Vec<IntegrationEvent> = best_by_key.into_values().collect();
    reduced.sort_by(|a, b| calendar_updated_value(b).cmp(&calendar_updated_value(a)));

    if reduced.len() > max_events {
        reduced.truncate(max_events);
    }

    let omitted_count = events.len().saturating_sub(reduced.len());
    (reduced, omitted_count)
}

fn is_low_value_calendar_event(event: &IntegrationEvent) -> bool {
    let status = event
        .payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !status.eq_ignore_ascii_case("cancelled") {
        return false;
    }

    let summary = event
        .payload
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim();
    let start = event
        .payload
        .get("start")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    summary.eq_ignore_ascii_case("Untitled")
        && (start.starts_with("2000-01-01") || start.starts_with("2000-01-02") || start.is_empty())
}

fn calendar_logical_key(event: &IntegrationEvent) -> String {
    let payload = &event.payload;
    let calendar_id = payload
        .get("calendar_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let summary = payload
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let status = payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let start = payload
        .get("start")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let end = payload
        .get("end")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    format!(
        "{}|{}|{}|{}|{}|{}",
        event.account_id, calendar_id, summary, status, start, end
    )
}

fn calendar_updated_value(event: &IntegrationEvent) -> String {
    event
        .payload
        .get("updated")
        .and_then(|v| v.as_str())
        .unwrap_or(&event.created_at)
        .to_string()
}

async fn mark_events_processed(
    state: &Arc<AppState>,
    events: &[IntegrationEvent],
) -> anyhow::Result<()> {
    for event in events {
        state.db.mark_integration_event_processed(&event.id).await?;
    }
    Ok(())
}

async fn mark_events_new(state: &Arc<AppState>, events: &[IntegrationEvent]) -> anyhow::Result<()> {
    for event in events {
        state.db.mark_integration_event_new(&event.id).await?;
    }
    Ok(())
}

/// Extract entity names from integration events
fn extract_event_entities(event: &IntegrationEvent) -> Vec<String> {
    let mut entities = Vec::new();

    match event.event_type.as_str() {
        "new_email" | "gmail_new_message" => {
            extract_email_entities(&event.payload, &mut entities);
        }
        "new_email_batch" => {
            if let Some(emails) = event.payload.get("emails").and_then(|v| v.as_array()) {
                for email_event in emails {
                    if let Some(payload) = email_event.get("payload") {
                        extract_email_entities(payload, &mut entities);
                    }
                }
            }
        }
        "calendar_event" | "calendar_event_updated" => {
            extract_calendar_entities(&event.payload, &mut entities);
        }
        "calendar_event_batch" => {
            if let Some(items) = event.payload.get("events").and_then(|v| v.as_array()) {
                for item in items {
                    if let Some(payload) = item.get("payload") {
                        extract_calendar_entities(payload, &mut entities);
                    }
                }
            }
        }
        _ => {}
    }

    entities
}

fn extract_calendar_entities(payload: &serde_json::Value, entities: &mut Vec<String>) {
    if let Some(attendees) = payload.get("attendees").and_then(|v| v.as_array()) {
        for attendee in attendees {
            if let Some(email) = attendee.get("email").and_then(|v| v.as_str()) {
                entities.push(email.to_string());
            }
            if let Some(name) = attendee.get("displayName").and_then(|v| v.as_str()) {
                entities.push(name.to_string());
            }
        }
    }

    if let Some(summary) = payload.get("summary").and_then(|v| v.as_str()) {
        if !summary.trim().is_empty() {
            entities.push(summary.to_string());
        }
    }

    if let Some(account_email) = payload.get("account_email").and_then(|v| v.as_str()) {
        if !account_email.trim().is_empty() {
            entities.push(account_email.to_string());
        }
    }

    if let Some(location) = payload.get("location").and_then(|v| v.as_str()) {
        if !location.is_empty() {
            entities.push(location.to_string());
        }
    }
}

fn extract_email_entities(payload: &serde_json::Value, entities: &mut Vec<String>) {
    // Extract sender
    if let Some(from) = payload.get("from").and_then(|v| v.as_str()) {
        if let Some(name_part) = from.split('<').next() {
            let cleaned = name_part.trim().trim_matches('"');
            if !cleaned.is_empty() && cleaned != from {
                entities.push(cleaned.to_string());
            }
        }
        entities.push(from.to_string());
    }

    // Extract recipients
    if let Some(to) = payload.get("to").and_then(|v| v.as_array()) {
        for recipient in to {
            if let Some(addr) = recipient.as_str() {
                entities.push(addr.to_string());
            }
        }
    }
}

