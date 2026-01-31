use std::sync::Arc;
use chrono::Utc;
use jossie_server::AppState;
use jossie_db::{IntegrationEvent, IntegrationAccount};
use jossie_core::types::{Message, Role};
use uuid::Uuid;

const POLL_INTERVAL_SECS: u64 = 120;
const PENDING_LIMIT: usize = 20;

pub async fn start_event_loop(state: Arc<AppState>) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(POLL_INTERVAL_SECS));
    loop {
        if let Err(e) = poll_google_events(&state).await {
            tracing::error!("Google event poll failed: {e}");
        }
        if let Err(e) = process_pending_events(&state).await {
            tracing::error!("Event processing failed: {e}");
        }
        interval.tick().await;
    }
}

async fn poll_google_events(state: &Arc<AppState>) -> anyhow::Result<()> {
    let Some(google) = state.google_integration.as_ref() else {
        return Ok(());
    };

    let accounts = state.db.list_integration_accounts("google").await?;
    for acc in accounts {
        if let Err(e) = poll_gmail_for_account(state, google, &acc).await {
            tracing::warn!("Gmail poll failed for account {}: {e}", acc.id);
        }
        if let Err(e) = poll_calendar_for_account(state, google, &acc).await {
            tracing::warn!("Calendar poll failed for account {}: {e}", acc.id);
        }
    }

    Ok(())
}

async fn poll_gmail_for_account(
    state: &Arc<AppState>,
    google: &Arc<jossie_integration_google::GoogleIntegration>,
    acc: &IntegrationAccount,
) -> anyhow::Result<()> {
    let history_key = format!("gmail_history_id:{}", acc.id);
    let history_id = match state.db.get_integration_setting("google", &history_key).await? {
        Some(val) => val,
        None => {
            let profile = google.gmail_get_profile(&acc.id).await?;
            state.db.set_integration_setting("google", &history_key, &profile.history_id).await?;
            return Ok(());
        }
    };

    match google.gmail_list_history(&acc.id, &history_id).await? {
        jossie_integration_google::GmailHistoryOutcome::Reset { history_id } => {
            state.db.set_integration_setting("google", &history_key, &history_id).await?;
            return Ok(());
        }
        jossie_integration_google::GmailHistoryOutcome::Updated(result) => {
            let account_email = account_email(acc);
            for msg in result.messages {
                let payload = serde_json::json!({
                    "message_id": msg.id,
                    "thread_id": msg.thread_id,
                    "from": msg.from,
                    "subject": msg.subject,
                    "date": msg.date,
                    "snippet": msg.snippet,
                    "account_id": acc.id,
                    "account_email": account_email,
                });
                let _ = state
                    .db
                    .insert_integration_event(
                        "google",
                        &acc.id,
                        "gmail_new_message",
                        &msg.id,
                        &payload,
                    )
                    .await?;
            }
            state.db.set_integration_setting("google", &history_key, &result.history_id).await?;
        }
    }

    Ok(())
}

async fn poll_calendar_for_account(
    state: &Arc<AppState>,
    google: &Arc<jossie_integration_google::GoogleIntegration>,
    acc: &IntegrationAccount,
) -> anyhow::Result<()> {
    let updated_key = format!("calendar_updated_min:{}", acc.id);
    let updated_min = match state.db.get_integration_setting("google", &updated_key).await? {
        Some(val) => val,
        None => {
            let now = Utc::now().to_rfc3339();
            state.db.set_integration_setting("google", &updated_key, &now).await?;
            return Ok(());
        }
    };

    let events = google.calendar_list_updated_events(&acc.id, &updated_min).await?;
    let account_email = account_email(acc);
    let mut max_updated = updated_min.clone();

    for ev in events {
        if ev.updated > max_updated {
            max_updated = ev.updated.clone();
        }
        let dedupe_key = format!("{}:{}", ev.id, ev.updated);
        let payload = serde_json::json!({
            "event_id": ev.id,
            "summary": ev.summary,
            "start": ev.start,
            "end": ev.end,
            "status": ev.status,
            "updated": ev.updated,
            "location": ev.location,
            "account_id": acc.id,
            "account_email": account_email,
        });
        let _ = state
            .db
            .insert_integration_event(
                "google",
                &acc.id,
                "calendar_event_updated",
                &dedupe_key,
                &payload,
            )
            .await?;
    }

    if max_updated != updated_min {
        state.db.set_integration_setting("google", &updated_key, &max_updated).await?;
    }

    Ok(())
}

async fn process_pending_events(state: &Arc<AppState>) -> anyhow::Result<()> {
    if state.telegram_token.trim().is_empty() {
        return Ok(());
    }

    let Some(chat) = state.db.get_latest_telegram_chat().await? else {
        return Ok(());
    };

    let events = state.db.list_pending_integration_events(PENDING_LIMIT).await?;
    for event in events {
        if let Err(e) = handle_event(state, &chat, &event).await {
            state.db.mark_integration_event_failed(&event.id, &e.to_string()).await?;
        }
    }

    Ok(())
}

async fn handle_event(
    state: &Arc<AppState>,
    chat: &jossie_db::TelegramChatLink,
    event: &IntegrationEvent,
) -> anyhow::Result<()> {
    let message = jossie_server::agent::generate_event_message(state, chat.conversation_id, event).await?;
    let Some(message) = message else {
        state.db.mark_integration_event_processed(&event.id).await?;
        return Ok(());
    };

    jossie_telegram::send_message(&state.telegram_token, chat.chat_id, &message).await?;

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

fn account_email(acc: &IntegrationAccount) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(&acc.data).ok()?;
    value.get("email").and_then(|v| v.as_str()).map(|s| s.to_string())
}
