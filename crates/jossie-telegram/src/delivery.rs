fn spawn_typing(bot: Bot, chat_id: ChatId) -> oneshot::Sender<()> {
    spawn_typing_with_interval(bot, chat_id, TYPING_REFRESH_INTERVAL)
}

fn spawn_typing_with_interval(
    bot: Bot,
    chat_id: ChatId,
    refresh_interval: Duration,
) -> oneshot::Sender<()> {
    let (stop_tx, mut stop_rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(refresh_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = interval.tick() => {
                    if let Err(error) = bot.send_chat_action(chat_id, ChatAction::Typing).await {
                        tracing::debug!(chat_id = chat_id.0, "Failed to refresh Telegram typing status: {error}");
                    }
                }
            }
        }
    });
    stop_tx
}

async fn send_reply(
    bot: &Bot,
    chat_id: ChatId,
    reply_to: Option<MessageId>,
    text: &str,
    keyboard: Option<InlineKeyboardMarkup>,
) -> Result<(), teloxide::RequestError> {
    let text = if text.trim().is_empty() {
        "I couldn't produce a response. Please try again."
    } else {
        text
    };
    let chunks = split_message(text, TELEGRAM_MESSAGE_LIMIT);
    for (index, chunk) in chunks.iter().enumerate() {
        let mut request = bot.send_message(chat_id, chunk.clone());
        if let Some(message_id) = reply_to {
            request = request
                .reply_parameters(ReplyParameters::new(message_id).allow_sending_without_reply());
        }
        if index + 1 == chunks.len()
            && let Some(markup) = keyboard.clone()
        {
            request = request.reply_markup(markup);
        }
        match request.await {
            Ok(_) => {}
            Err(teloxide::RequestError::RetryAfter(seconds)) => {
                tokio::time::sleep(seconds.duration()).await;
                let mut retry = bot.send_message(chat_id, chunk.clone());
                if let Some(message_id) = reply_to {
                    retry = retry.reply_parameters(
                        ReplyParameters::new(message_id).allow_sending_without_reply(),
                    );
                }
                if index + 1 == chunks.len()
                    && let Some(markup) = keyboard.clone()
                {
                    retry = retry.reply_markup(markup);
                }
                retry.await?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub async fn send_message(token: &str, chat_id: i64, text: &str) -> anyhow::Result<()> {
    if token.trim().is_empty() {
        anyhow::bail!("Telegram bot token is missing");
    }
    send_reply(&Bot::new(token.trim()), ChatId(chat_id), None, text, None).await?;
    Ok(())
}

async fn send_generic_error(
    bot: &Bot,
    msg: &teloxide::types::Message,
) -> Result<(), teloxide::RequestError> {
    send_reply(
        bot,
        msg.chat.id,
        Some(msg.id),
        "I couldn't finish that just now. Please try again.",
        None,
    )
    .await
}

fn user_facing_error(error: &anyhow::Error) -> &'static str {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("unsupported telegram document") {
        "I can currently read photos, PDFs, common office documents, spreadsheets, text, and source-code files."
    } else if message.contains("too large") || message.contains("exceeds the configured limit") {
        "That attachment is too large. Telegram media is limited to 20 MB per message or album."
    } else if message.contains("transcription is disabled") {
        "Voice and audio transcription is disabled in the current configuration."
    } else if message.contains("ffmpeg") {
        "Voice notes are temporarily unavailable because FFmpeg is not installed or configured."
    } else if message.contains("run cancelled") {
        "Stopped."
    } else {
        "I couldn't finish that just now. Please try again."
    }
}

fn command_from_message(msg: &teloxide::types::Message) -> Option<Command> {
    let text = msg.text()?;
    let command = text.split_whitespace().next()?;
    let command = command.split('@').next()?.to_ascii_lowercase();
    match command.as_str() {
        "/start" => Some(Command::Start),
        "/help" => Some(Command::Help),
        "/new" => Some(Command::New),
        "/cancel" => Some(Command::Cancel),
        "/status" | "/goals" => Some(Command::Status),
        "/resume" => Some(Command::Resume),
        _ => None,
    }
}

fn authorized_message(msg: &teloxide::types::Message, allowed_user_id: Option<i64>) -> bool {
    let Some(user) = msg.from.as_ref() else {
        tracing::warn!(
            chat_id = msg.chat.id.0,
            "Ignoring Telegram message without a sender"
        );
        return false;
    };
    authorized_user_id(user.id.0 as i64, allowed_user_id)
}

fn authorized_user_id(user_id: i64, allowed_user_id: Option<i64>) -> bool {
    let authorized = allowed_user_id.is_none_or(|allowed| allowed == user_id);
    if !authorized {
        tracing::warn!(
            user_id,
            allowed_user_id,
            "Ignoring unauthorized Telegram update"
        );
    }
    authorized
}

async fn try_activate_chat(runtime: &TelegramRuntime, chat_id: i64) -> bool {
    runtime.active_chats.lock().await.insert(chat_id)
}

async fn release_chat(runtime: &TelegramRuntime, chat_id: i64) {
    runtime.active_chats.lock().await.remove(&chat_id);
}

async fn chat_is_active(runtime: &TelegramRuntime, chat_id: i64) -> bool {
    runtime.active_chats.lock().await.contains(&chat_id)
}

fn respond_ok() -> Result<(), teloxide::RequestError> {
    Ok(())
}

fn split_message(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut remaining = text;
    let mut chunks = Vec::new();
    while remaining.chars().count() > max_chars {
        let limit_byte = remaining
            .char_indices()
            .nth(max_chars)
            .map(|(index, _)| index)
            .unwrap_or(remaining.len());
        let prefix = &remaining[..limit_byte];
        let split_at = prefix
            .rfind('\n')
            .filter(|index| *index > 0)
            .or_else(|| prefix.rfind(char::is_whitespace).filter(|index| *index > 0))
            .unwrap_or(limit_byte);
        let (chunk, rest) = remaining.split_at(split_at);
        chunks.push(chunk.trim_end().to_string());
        remaining = rest.trim_start();
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }
    chunks
}

