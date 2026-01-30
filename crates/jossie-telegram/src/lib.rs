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
        use teloxide::types::ParseMode;

        let bot = Bot::new(&self.token);
        let state = self.state;
        let chat_map = self.chat_map;

        let handler = Update::filter_message().endpoint(
            move |bot: Bot, msg: Message| {
                let state = state.clone();
                let chat_map = chat_map.clone();
                async move {
                    let Some(text) = msg.text() else {
                        return Ok(());
                    };

                    let chat_id = msg.chat.id.0;

                    // Get or create conversation for this chat
                    let conv_id = {
                        let map = chat_map.read().await;
                        map.get(&chat_id).copied()
                    };

                    let conv_id = match conv_id {
                        Some(id) => id,
                        None => {
                            let title = format!("Telegram chat {}", chat_id);
                            let conv = state.db.create_conversation(Some(&title)).await
                                .map_err(|e| {
                                    tracing::error!("Failed to create conversation: {e}");
                                    e
                                })?;
                            chat_map.write().await.insert(chat_id, conv.id);
                            conv.id
                        }
                    };

                    // Save user message
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
                    state.db.save_message(&user_msg).await?;

                    // Run agent loop
                    let response = jossie_server::run_agent_loop(&state, conv_id).await
                        .unwrap_or_else(|e| format!("Error: {e}"));

                    // Split long messages (Telegram 4096 char limit)
                    let chunks: Vec<&str> = if response.len() <= 4096 {
                        vec![&response]
                    } else {
                        response.as_bytes()
                            .chunks(4096)
                            .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
                            .collect()
                    };

                    for chunk in chunks {
                        if !chunk.is_empty() {
                            bot.send_message(msg.chat.id, chunk).await?;
                        }
                    }

                    Ok(())
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
