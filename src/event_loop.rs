use chrono::Utc;
use jossie_core::types::{Message, Role};
use jossie_db::IntegrationEvent;
use jossie_server::AppState;
use jossie_server::events::{ServerEvent, persist_message};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

const INTEGRATION_POLL_INTERVAL_SECS: u64 = 120;
const BACKGROUND_WORK_INTERVAL_SECS: u64 = 5;
const PENDING_LIMIT: usize = 20;
const CALENDAR_BATCH_MAX_EVENTS: usize = 50;
const STALE_PROCESSING_EVENT_TIMEOUT_SECS: i64 = 60 * 60;

#[derive(Clone, Copy, Debug)]
struct BackgroundTarget {
    conversation_id: Uuid,
    telegram_chat_id: Option<i64>,
}

pub async fn start_event_loop(state: Arc<AppState>) -> anyhow::Result<()> {
    recover_stale_processing_events(&state).await?;

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
                if let Err(e) = integration.poll().await {
                    tracing::error!("Poll failed for integration {}: {}", integration.name(), e);
                }
            }
            next_integration_poll = now + integration_poll_interval;
        }

        if let Err(e) = process_pending_events(&state).await {
            tracing::error!("Event processing failed: {e}");
        }

        if let Err(e) = process_scheduled_tasks(&state).await {
            tracing::error!("Scheduled task processing failed: {e}");
        }

        if let Err(e) = process_oob_messages(&state).await {
            tracing::error!("OOB message processing failed: {e}");
        }

        interval.tick().await;
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

    // If we fail anywhere below, reset the event status to 'new' so it can be retried
    let result = process_event_inner(state, target, event).await;

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            // Reset event to 'new' status on failure so it can be retried
            state
                .db
                .mark_integration_event_failed(&event.id, &e.to_string())
                .await?;
            Err(e)
        }
    }
}

async fn process_event_inner(
    state: &Arc<AppState>,
    target: &BackgroundTarget,
    event: &IntegrationEvent,
) -> anyhow::Result<()> {
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
                return Ok(());
            }
            Err(e) => return Err(e),
        };

    tracing::info!("Generated message: {:?}", message);
    let Some(message) = message else {
        state.db.mark_integration_event_processed(&event.id).await?;
        return Ok(());
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
        created_at: Utc::now(),
    };
    persist_message(state, &assistant_msg).await?;
    state.publish_event(ServerEvent::BackgroundNotification {
        conversation_id: target.conversation_id,
        source: "integration_event".to_string(),
        message: assistant_msg.content.clone(),
    });
    state.db.mark_integration_event_processed(&event.id).await?;
    Ok(())
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
        if let Some(max) = task.max_runs {
            if task.run_count >= max {
                state.db.mark_task_completed(&task.id).await?;
                tracing::info!("Task {} completed (max runs reached)", task.id);
                continue;
            }
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
            if !state.telegram_token.trim().is_empty() {
                if let Some(chat) = state.db.get_latest_telegram_chat().await? {
                    if chat.conversation_id == conversation_id {
                        jossie_telegram::send_message(
                            &state.telegram_token,
                            chat.chat_id,
                            &response,
                        )
                        .await?;
                    }
                }
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
        _ => {
            anyhow::bail!("Unknown schedule type: {}", task.schedule_type);
        }
    }

    Ok(())
}

async fn process_oob_messages(state: &Arc<AppState>) -> anyhow::Result<()> {
    let messages = state.db.list_pending_oob_messages(20).await?;
    let mut chat_cache: HashMap<Uuid, Option<i64>> = HashMap::new();

    for msg in messages {
        tracing::info!("Sending OOB message: {}", msg.id);

        let conversation_id: Uuid = msg.conversation_id.parse()?;

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

        if !state.telegram_token.trim().is_empty() {
            if let Some(chat_id) = chat_id {
                match jossie_telegram::send_message(&state.telegram_token, chat_id, &msg.content)
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
            created_at: Utc::now(),
        };
        persist_message(state, &assistant_msg).await?;
        state.publish_event(ServerEvent::BackgroundNotification {
            conversation_id,
            source: "oob_message".to_string(),
            message: assistant_msg.content.clone(),
        });
        state.db.mark_oob_message_sent(&msg.id).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_event(
        id: &str,
        event_type: &str,
        integration: &str,
        account_id: &str,
        payload: serde_json::Value,
    ) -> IntegrationEvent {
        IntegrationEvent {
            id: id.to_string(),
            integration: integration.to_string(),
            account_id: account_id.to_string(),
            event_type: event_type.to_string(),
            dedupe_key: format!("dedupe_{id}"),
            payload,
            status: "new".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            processed_at: None,
            last_error: None,
        }
    }

    #[test]
    fn identifies_email_event_types() {
        let email_event = mk_event("1", "new_email", "email", "acc_1", serde_json::json!({}));
        let gmail_event = mk_event(
            "2",
            "gmail_new_message",
            "google",
            "acc_2",
            serde_json::json!({}),
        );
        let calendar_event = mk_event(
            "3",
            "calendar_event",
            "google",
            "acc_2",
            serde_json::json!({}),
        );

        assert!(is_email_event(&email_event));
        assert!(is_email_event(&gmail_event));
        assert!(!is_email_event(&calendar_event));
    }

    #[test]
    fn identifies_calendar_event_types() {
        let calendar_event = mk_event(
            "1",
            "calendar_event_updated",
            "google",
            "acc_1",
            serde_json::json!({}),
        );
        let email_event = mk_event(
            "2",
            "gmail_new_message",
            "google",
            "acc_1",
            serde_json::json!({}),
        );

        assert!(is_calendar_event(&calendar_event));
        assert!(!is_calendar_event(&email_event));
    }

    #[test]
    fn builds_single_batched_email_event() {
        let e1 = mk_event(
            "1",
            "gmail_new_message",
            "google",
            "acc_1",
            serde_json::json!({
                "from": "alice@example.com",
                "subject": "Subject 1"
            }),
        );
        let e2 = mk_event(
            "2",
            "gmail_new_message",
            "google",
            "acc_1",
            serde_json::json!({
                "from": "bob@example.com",
                "subject": "Subject 2"
            }),
        );

        let batch = build_email_batch_event(&[e1, e2]);
        assert_eq!(batch.event_type, "new_email_batch");
        assert_eq!(batch.integration, "google");
        assert_eq!(batch.account_id, "acc_1");
        assert_eq!(batch.payload["count"], serde_json::json!(2));
        assert_eq!(batch.payload["emails"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn batches_mixed_sources_as_mixed() {
        let e1 = mk_event("1", "new_email", "email", "acc_1", serde_json::json!({}));
        let e2 = mk_event(
            "2",
            "gmail_new_message",
            "google",
            "acc_2",
            serde_json::json!({}),
        );

        let batch = build_email_batch_event(&[e1, e2]);
        assert_eq!(batch.integration, "mixed");
        assert_eq!(batch.account_id, "mixed");
    }

    #[test]
    fn reduces_calendar_events_with_dedupe_and_filtering() {
        let old = mk_event(
            "old",
            "calendar_event_updated",
            "google",
            "acc_1",
            serde_json::json!({
                "calendar_id": "primary",
                "summary": "Standup",
                "status": "confirmed",
                "start": "2026-02-10T10:00:00Z",
                "end": "2026-02-10T10:15:00Z",
                "updated": "2026-02-09T10:00:00Z"
            }),
        );
        let new = mk_event(
            "new",
            "calendar_event_updated",
            "google",
            "acc_1",
            serde_json::json!({
                "calendar_id": "primary",
                "summary": "Standup",
                "status": "confirmed",
                "start": "2026-02-10T10:00:00Z",
                "end": "2026-02-10T10:15:00Z",
                "updated": "2026-02-09T11:00:00Z"
            }),
        );
        let low_value = mk_event(
            "noise",
            "calendar_event_updated",
            "google",
            "acc_1",
            serde_json::json!({
                "summary": "Untitled",
                "status": "cancelled",
                "start": "2000-01-01T00:00:00Z",
                "updated": "2026-02-09T12:00:00Z"
            }),
        );

        let (reduced, omitted) = reduce_calendar_events(&[old, new.clone(), low_value], 50);
        assert_eq!(reduced.len(), 1);
        assert_eq!(omitted, 2);
        assert_eq!(reduced[0].id, new.id);
    }
}
