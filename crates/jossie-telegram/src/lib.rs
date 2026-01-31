use std::sync::Arc;
use jossie_server::AppState;
use uuid::Uuid;

pub struct TelegramBot {
    token: String,
    state: Arc<AppState>,
}

impl TelegramBot {
    pub fn new(token: &str, state: Arc<AppState>) -> Self {
        Self {
            token: token.trim().to_string(),
            state,
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.token.is_empty()
    }

    pub async fn run(self) -> anyhow::Result<()> {
        use teloxide::prelude::*;

        let bot = Bot::new(&self.token);
        let state = self.state;

        let handler = Update::filter_message().endpoint(
            move |bot: Bot, msg: Message| {
                let state = state.clone();
                async move {
                    let respond = |e: &dyn std::fmt::Display| -> teloxide::RequestError {
                        tracing::error!("Telegram handler error: {e}");
                        teloxide::RequestError::Api(teloxide::ApiError::Unknown(e.to_string()))
                    };

                    let Some(text) = msg.text() else {
                        return respond_ok();
                    };

                    let chat_id = msg.chat.id.0;
                    tracing::info!("Received Telegram message from {chat_id}: {:.20}...", text);

                    let conv_id = match state.db.get_telegram_conversation(chat_id).await {
                        Ok(Some(id)) => {
                            tracing::debug!("Found existing conversation {id} for chat {chat_id}");
                            id
                        },
                        Ok(None) => {
                            tracing::info!("Creating new conversation for chat {chat_id}");
                            let title = format!("Telegram chat {}", chat_id);
                            match state.db.create_conversation(Some(&title)).await {
                                Ok(conv) => {
                                    if let Err(e) = state.db.link_telegram_conversation(chat_id, conv.id).await {
                                        return Err(respond(&e));
                                    }
                                    conv.id
                                }
                                Err(e) => return Err(respond(&e)),
                            }
                        }
                        Err(e) => return Err(respond(&e)),
                    };

                    let user_msg = jossie_core::types::Message {
                        id: Uuid::new_v4(),
                        conversation_id: conv_id,
                        role: jossie_core::types::Role::User,
                        content: text.to_string(),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                        created_at: chrono::Utc::now(),
                    };
                    if let Err(e) = state.db.save_message(&user_msg).await {
                        return Err(respond(&e));
                    }

                    tracing::info!("Running agent loop for conversation {conv_id}...");
                    let response = match jossie_server::run_agent_loop(&state, conv_id).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!("Agent loop failed: {e}");
                            format!("Error: {e}")
                        }
                    };
                    tracing::info!("Agent response length: {}", response.len());

                    // Split long messages (Telegram 4096 char limit)
                    for chunk in split_message(&response, 4096) {
                        if let Err(e) = bot.send_message(msg.chat.id, chunk).await {
                            tracing::error!("Failed to send message to {chat_id}: {e}");
                            return Err(e);
                        }
                    }
                    tracing::info!("Response sent to {chat_id}");

                    respond_ok()
                }
            },
        );

        tracing::info!("Starting Telegram bot... Instance ID: {}", Uuid::new_v4());
        bot.delete_webhook().await?;
        Dispatcher::builder(bot, handler)
            .build()
            .dispatch()
            .await;

        Ok(())
    }
}

fn respond_ok() -> Result<(), teloxide::RequestError> {
    Ok(())
}

fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
        // Try to split at a char boundary
        let end = if end < text.len() {
            (start..end).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(end)
        } else {
            end
        };
        if end <= start {
            break;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}
