use std::collections::HashMap;
use std::sync::Arc;
use jossie_server::AppState;
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct TelegramBot {
    token: String,
    state: Arc<AppState>,
    chat_map: Arc<RwLock<HashMap<i64, Uuid>>>,
}

impl TelegramBot {
    pub fn new(token: &str, state: Arc<AppState>) -> Self {
        Self {
            token: token.to_string(),
            state,
            chat_map: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.token.is_empty()
    }

    pub async fn run(self) -> anyhow::Result<()> {
        use teloxide::prelude::*;

        let bot = Bot::new(&self.token);
        let state = self.state;
        let chat_map = self.chat_map;

        let handler = Update::filter_message().endpoint(
            move |bot: Bot, msg: Message| {
                let state = state.clone();
                let chat_map = chat_map.clone();
                async move {
                    let respond = |e: &dyn std::fmt::Display| -> teloxide::RequestError {
                        tracing::error!("{e}");
                        teloxide::RequestError::Api(teloxide::ApiError::Unknown(e.to_string()))
                    };

                    let Some(text) = msg.text() else {
                        return respond_ok();
                    };

                    let chat_id = msg.chat.id.0;

                    let conv_id = {
                        let map = chat_map.read().await;
                        map.get(&chat_id).copied()
                    };

                    let conv_id = match conv_id {
                        Some(id) => id,
                        None => {
                            let title = format!("Telegram chat {}", chat_id);
                            match state.db.create_conversation(Some(&title)).await {
                                Ok(conv) => {
                                    chat_map.write().await.insert(chat_id, conv.id);
                                    conv.id
                                }
                                Err(e) => return Err(respond(&e)),
                            }
                        }
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

                    let response = jossie_server::run_agent_loop(&state, conv_id).await
                        .unwrap_or_else(|e| format!("Error: {e}"));

                    // Split long messages (Telegram 4096 char limit)
                    for chunk in split_message(&response, 4096) {
                        bot.send_message(msg.chat.id, chunk).await?;
                    }

                    respond_ok()
                }
            },
        );

        tracing::info!("Starting Telegram bot...");
        Dispatcher::builder(bot, handler)
            .enable_ctrlc_handler()
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
