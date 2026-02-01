use chrono::Utc;
use jossie_core::types::{Message, Role};
use jossie_db::{IntegrationAccount, IntegrationEvent};
use jossie_server::AppState;
use std::sync::Arc;
use uuid::Uuid;

const POLL_INTERVAL_SECS: u64 = 120;
const PENDING_LIMIT: usize = 20;

pub async fn start_event_loop(state: Arc<AppState>) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(POLL_INTERVAL_SECS));
    loop {
        tracing::info!("Event loop iteration");
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
            tracing::warn!(
                "Caaccount_emaillendar poll failed for account {}: {e}",
                acc.id
            );
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
    let history_id = match state
        .db
        .get_integration_setting("google", &history_key)
        .await?
    {
        Some(val) => val,
        None => {
            let profile = google.gmail_get_profile(&acc.id).await?;
            state
                .db
                .set_integration_setting("google", &history_key, &profile.history_id)
                .await?;
            return Ok(());
        }
    };

    match google.gmail_list_history(&acc.id, &history_id).await? {
        jossie_integration_google::GmailHistoryOutcome::Reset { history_id } => {
            state
                .db
                .set_integration_setting("google", &history_key, &history_id)
                .await?;
            return Ok(());
        }
        jossie_integration_google::GmailHistoryOutcome::Updated(result) => {
            let account_email = account_email(acc);
            for msg in result.messages {
                tracing::info!("New Gmail message: {}", msg.id);
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
            state
                .db
                .set_integration_setting("google", &history_key, &result.history_id)
                .await?;
        }
    }

    Ok(())
}

async fn poll_calendar_for_account(
    state: &Arc<AppState>,
    google: &Arc<jossie_integration_google::GoogleIntegration>,
    acc: &IntegrationAccount,
) -> anyhow::Result<()> {
    let calendars = match google.calendar_list_calendars(&acc.id).await {
        Ok(cals) => cals,
        Err(e) => {
            tracing::error!("Failed to list calendars for account {}: {}", acc.id, e);
            return Err(e);
        }
    };

    let account_email = account_email(acc);

    for calendar in calendars {
        let calendar_id = &calendar.id;
        let updated_key = format!("calendar_updated_min:{}:{}", acc.id, calendar_id);

        // Handle legacy key "calendar_updated_min:{acc.id}" for primary calendar
        let db_key = if calendar.primary {
            // If we have a legacy key and no new key, migrate/use it?
            // Simplest approach: check specific key first, fallback to general if primary.
            // Actually, let's just checking the specific key. If it's empty, and it is primary,
            // we could try to read the old key to avoid re-syncing everything.
            updated_key.clone()
        } else {
            updated_key.clone()
        };

        let updated_min = match state.db.get_integration_setting("google", &db_key).await? {
            Some(val) => val,
            None => {
                // If this is primary, check if we have the old legacy key
                if calendar.primary {
                    if let Some(val) = state
                        .db
                        .get_integration_setting(
                            "google",
                            &format!("calendar_updated_min:{}", acc.id),
                        )
                        .await?
                    {
                        val
                    } else {
                        // Default to now
                        let now = Utc::now().to_rfc3339();
                        state
                            .db
                            .set_integration_setting("google", &db_key, &now)
                            .await?;
                        now
                    }
                } else {
                    let now = Utc::now().to_rfc3339();
                    state
                        .db
                        .set_integration_setting("google", &db_key, &now)
                        .await?;
                    now
                }
            }
        };

        match google
            .calendar_list_updated_events(&acc.id, calendar_id, &updated_min)
            .await
        {
            Ok(events) => {
                let mut max_updated = updated_min.clone();
                for ev in events {
                    if ev.updated > max_updated {
                        max_updated = ev.updated.clone();
                    }
                    let dedupe_key = format!("{}:{}:{}", calendar_id, ev.id, ev.updated);
                    let payload = serde_json::json!({
                        "event_id": ev.id,
                        "calendar_id": calendar_id,
                        "calendar_summary": calendar.summary,
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
                    state
                        .db
                        .set_integration_setting("google", &db_key, &max_updated)
                        .await?;
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to poll calendar {} for account {}: {}",
                    calendar_id,
                    acc.id,
                    e
                );
            }
        }
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

fn account_email(acc: &IntegrationAccount) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(&acc.data).ok()?;
    value
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
