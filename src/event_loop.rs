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
    for event in events {
        tracing::info!("Processing event: {}", event.id);
        if let Err(e) = handle_event(state, &chat, &event).await {
            tracing::error!("Event processing failed for {}: {}", event.id, e);
            state
                .db
                .mark_integration_event_failed(&event.id, &e.to_string())
                .await?;
        }
    }

    Ok(())
}

async fn handle_event(
    state: &Arc<AppState>,
    chat: &jossie_db::TelegramChatLink,
    event: &IntegrationEvent,
) -> anyhow::Result<()> {
    tracing::info!("Processing event: {}", event.id);
    let message =
        jossie_server::agent::generate_event_message(state, chat.conversation_id, event).await?;
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
