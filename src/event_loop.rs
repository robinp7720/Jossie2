use chrono::Utc;
use jossie_core::types::{Message, Role};
use jossie_db::IntegrationEvent;
use jossie_server::AppState;
use std::sync::Arc;
use uuid::Uuid;

const POLL_INTERVAL_SECS: u64 = 120;
const PENDING_LIMIT: usize = 20;

pub async fn start_event_loop(state: Arc<AppState>) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(POLL_INTERVAL_SECS));
    loop {
        tracing::info!("Event loop iteration");

        for integration in state.registry.get_integrations() {
            if let Err(e) = integration.poll().await {
                tracing::error!("Poll failed for integration {}: {}", integration.name(), e);
            }
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

async fn process_pending_events(state: &Arc<AppState>) -> anyhow::Result<()> {
    if state.telegram_token.trim().is_empty() {
        return Ok(());
    }

    let Some(chat) = state.db.get_latest_telegram_chat().await? else {
        return Ok(());
    };

    let events = state
        .db
        .list_pending_integration_events(PENDING_LIMIT)
        .await?;
    let mut email_events = Vec::new();
    for event in events {
        if is_email_event(&event) {
            email_events.push(event);
            continue;
        }

        tracing::info!("Processing event: {}", event.id);
        if let Err(e) = handle_event(state, &chat, &event).await {
            tracing::error!("Event processing failed for {}: {}", event.id, e);
            state
                .db
                .mark_integration_event_failed(&event.id, &e.to_string())
                .await?;
        }
    }

    if !email_events.is_empty() {
        tracing::info!("Batch processing {} email events", email_events.len());
        if let Err(e) = handle_email_event_batch(state, &chat, &email_events).await {
            tracing::error!("Email batch processing failed: {}", e);
        }
    }

    Ok(())
}

fn is_email_event(event: &IntegrationEvent) -> bool {
    matches!(event.event_type.as_str(), "new_email" | "gmail_new_message")
}

async fn handle_event(
    state: &Arc<AppState>,
    chat: &jossie_db::TelegramChatLink,
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
    let result = process_event_inner(state, chat, event).await;

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
    chat: &jossie_db::TelegramChatLink,
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

    let message = match jossie_server::agent::generate_event_message(
        state,
        chat.conversation_id,
        event,
    )
    .await
    {
        Ok(msg) => msg,
        Err(e) if e.to_string().contains("already being processed") => {
            // Conversation is busy with Telegram chat or another event, skip this event
            tracing::debug!(
                "Skipping event {} - conversation {} is busy",
                event.id,
                chat.conversation_id
            );
            state.db.mark_integration_event_processed(&event.id).await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    tracing::info!("Generated message: {:?}", message);
    let Some(message) = message else {
        state.db.mark_integration_event_processed(&event.id).await?;
        return Ok(());
    };

    tracing::info!("Sending message: {}", message);
    jossie_telegram::send_message(&state.telegram_token, chat.chat_id, &message).await?;
    tracing::info!("Message sent: {}", message);

    let assistant_msg = Message {
        id: Uuid::new_v4(),
        conversation_id: chat.conversation_id,
        role: Role::Assistant,
        content: message,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        created_at: Utc::now(),
    };
    state.db.save_message(&assistant_msg).await?;
    state.db.mark_integration_event_processed(&event.id).await?;
    Ok(())
}

async fn handle_email_event_batch(
    state: &Arc<AppState>,
    chat: &jossie_db::TelegramChatLink,
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

    let result = process_email_event_batch_inner(state, chat, &claimed_events).await;
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
    chat: &jossie_db::TelegramChatLink,
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
        chat.conversation_id,
        &batched_event,
    )
    .await
    {
        Ok(msg) => msg,
        Err(e) if e.to_string().contains("already being processed") => {
            tracing::debug!(
                "Skipping email batch - conversation {} is busy",
                chat.conversation_id
            );
            mark_events_processed(state, events).await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    tracing::info!("Generated batched email message: {:?}", message);
    let Some(message) = message else {
        mark_events_processed(state, events).await?;
        return Ok(());
    };

    tracing::info!("Sending batched email message: {}", message);
    jossie_telegram::send_message(&state.telegram_token, chat.chat_id, &message).await?;
    tracing::info!("Batched email message sent: {}", message);

    let assistant_msg = Message {
        id: Uuid::new_v4(),
        conversation_id: chat.conversation_id,
        role: Role::Assistant,
        content: message,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        created_at: Utc::now(),
    };
    state.db.save_message(&assistant_msg).await?;
    mark_events_processed(state, events).await?;

    Ok(())
}

fn build_email_batch_event(events: &[IntegrationEvent]) -> IntegrationEvent {
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

    let email_events: Vec<serde_json::Value> = events
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
        dedupe_key: format!("batch:{}:{}", events.len(), Uuid::new_v4()),
        payload: serde_json::json!({
            "count": events.len(),
            "emails": email_events,
        }),
        status: "processing".to_string(),
        created_at: Utc::now().to_rfc3339(),
        processed_at: None,
        last_error: None,
    }
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
        "calendar_event" => {
            // Extract attendees from calendar event
            if let Some(attendees) = event.payload.get("attendees").and_then(|v| v.as_array()) {
                for attendee in attendees {
                    if let Some(email) = attendee.get("email").and_then(|v| v.as_str()) {
                        entities.push(email.to_string());
                    }
                    if let Some(name) = attendee.get("displayName").and_then(|v| v.as_str()) {
                        entities.push(name.to_string());
                    }
                }
            }

            // Extract location if it's a company/place
            if let Some(location) = event.payload.get("location").and_then(|v| v.as_str()) {
                if !location.is_empty() {
                    entities.push(location.to_string());
                }
            }
        }
        _ => {}
    }

    entities
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
                created_at: Utc::now(),
            };
            state.db.save_message(&user_msg).await?;

            // Run the agent loop
            let response = jossie_server::agent::run_agent_loop(state, conversation_id).await?;

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
    if state.telegram_token.trim().is_empty() {
        return Ok(());
    }

    let messages = state.db.list_pending_oob_messages(20).await?;

    for msg in messages {
        tracing::info!("Sending OOB message: {}", msg.id);

        let conversation_id: Uuid = msg.conversation_id.parse()?;

        // Try to find the Telegram chat for this conversation
        let chat_id = if let Some(chat) = state.db.get_latest_telegram_chat().await? {
            if chat.conversation_id == conversation_id {
                Some(chat.chat_id)
            } else {
                None
            }
        } else {
            None
        };

        if let Some(chat_id) = chat_id {
            match jossie_telegram::send_message(&state.telegram_token, chat_id, &msg.content).await
            {
                Ok(_) => {
                    state.db.mark_oob_message_sent(&msg.id).await?;
                    tracing::info!("OOB message {} sent successfully", msg.id);
                }
                Err(e) => {
                    state
                        .db
                        .mark_oob_message_failed(&msg.id, &e.to_string())
                        .await?;
                    tracing::error!("Failed to send OOB message {}: {}", msg.id, e);
                }
            }
        } else {
            let err = "No Telegram chat found for conversation";
            state.db.mark_oob_message_failed(&msg.id, err).await?;
            tracing::warn!("Cannot send OOB message {}: {}", msg.id, err);
        }
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
}
