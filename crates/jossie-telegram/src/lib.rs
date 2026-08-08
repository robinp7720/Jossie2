use jossie_core::types::{Attachment, Message as JossieMessage, Role};
use jossie_server::AppState;
use jossie_server::handlers::chat::{PendingReply, pending_reply};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use teloxide::RequestError;
use teloxide::errors::ApiError;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, ChatAction, FileId, InlineKeyboardButton, InlineKeyboardMarkup, MessageId,
    ReplyParameters,
};
use teloxide::update_listeners::{self, UpdateListener};
use teloxide::utils::command::BotCommands;
use tokio::sync::{Mutex, oneshot};
use uuid::Uuid;

const TELEGRAM_MESSAGE_LIMIT: usize = 4096;
const TYPING_REFRESH_INTERVAL: Duration = Duration::from_secs(4);
const MEDIA_GROUP_DEBOUNCE: Duration = Duration::from_millis(750);

#[derive(Clone, BotCommands)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "show what Jossie can do")]
    Start,
    #[command(description = "show available commands")]
    Help,
    #[command(description = "start a fresh conversation")]
    New,
    #[command(description = "stop the current run")]
    Cancel,
}

#[derive(Default)]
struct TelegramRuntime {
    active_chats: Mutex<HashSet<i64>>,
    albums: Mutex<HashMap<(i64, String), PendingAlbum>>,
}

struct PendingAlbum {
    generation: u64,
    messages: Vec<teloxide::types::Message>,
}

#[derive(Clone)]
pub struct TelegramBot {
    token: String,
    state: Arc<AppState>,
    allowed_user_id: Option<i64>,
}

impl TelegramBot {
    pub fn new(token: &str, state: Arc<AppState>, allowed_user_id: Option<i64>) -> Self {
        Self {
            token: token.trim().to_string(),
            state,
            allowed_user_id,
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.token.is_empty()
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let bot = Bot::new(&self.token);
        let runtime = Arc::new(TelegramRuntime::default());
        let state = self.state;
        let allowed_user_id = self.allowed_user_id;

        if state.llm.transcription_is_configured() {
            match tokio::process::Command::new(&state.telegram_ffmpeg_path)
                .arg("-version")
                .output()
                .await
            {
                Ok(output) if output.status.success() => {}
                Ok(_) | Err(_) => tracing::warn!(
                    ffmpeg = %state.telegram_ffmpeg_path,
                    "FFmpeg is unavailable; Telegram voice notes will be rejected"
                ),
            }
        }

        let message_state = state.clone();
        let message_runtime = runtime.clone();
        let message_handler = Update::filter_message().endpoint(
            move |bot: Bot, msg: teloxide::types::Message| {
                let state = message_state.clone();
                let runtime = message_runtime.clone();
                async move {
                    if !authorized_message(&msg, allowed_user_id) {
                        return respond_ok();
                    }
                    if !msg.chat.is_private() {
                        if command_from_message(&msg).is_some() {
                            let _ = bot
                                .send_message(msg.chat.id, "Please message me in a private chat.")
                                .await;
                        }
                        return respond_ok();
                    }

                    if let Some(command) = command_from_message(&msg) {
                        handle_command(bot, state, runtime, msg, command).await;
                        return respond_ok();
                    }

                    if let Some(group_id) = msg.media_group_id().map(ToString::to_string) {
                        queue_album(bot, state, runtime, msg, group_id).await;
                        return respond_ok();
                    }

                    if !try_activate_chat(&runtime, msg.chat.id.0).await {
                        let _ = send_reply(
                            &bot,
                            msg.chat.id,
                            Some(msg.id),
                            "I'm still working on your previous message. Use /cancel if you want me to stop.",
                            None,
                        )
                        .await;
                        return respond_ok();
                    }

                    let chat_id = msg.chat.id.0;
                    tokio::spawn(async move {
                        process_turn(bot, state, vec![msg]).await;
                        release_chat(&runtime, chat_id).await;
                    });
                    respond_ok()
                }
            },
        );

        let callback_state = state.clone();
        let callback_runtime = runtime.clone();
        let callback_handler =
            Update::filter_callback_query().endpoint(move |bot: Bot, query: CallbackQuery| {
                let state = callback_state.clone();
                let runtime = callback_runtime.clone();
                async move {
                    if !authorized_user_id(query.from.id.0 as i64, allowed_user_id) {
                        return respond_ok();
                    }
                    tokio::spawn(async move {
                        handle_callback(bot, state, runtime, query).await;
                    });
                    respond_ok()
                }
            });

        let handler = dptree::entry()
            .branch(message_handler)
            .branch(callback_handler);

        tracing::info!(instance_id = %Uuid::new_v4(), "Starting Telegram bot");
        bot.delete_webhook().await?;
        bot.set_my_commands(Command::bot_commands()).await?;

        let mut listener = update_listeners::polling_default(bot.clone()).await;
        let stop_token = listener.stop_token();
        let polling_conflict = Arc::new(AtomicBool::new(false));
        let conflict_seen = polling_conflict.clone();
        let listener_error_handler = Arc::new(move |error: RequestError| {
            let stop_token = stop_token.clone();
            let conflict_seen = conflict_seen.clone();
            async move {
                if is_polling_conflict(&error) {
                    conflict_seen.store(true, Ordering::Release);
                    tracing::error!(
                        "Telegram polling stopped because another process is using this bot token; stop the other bot instance or configure a unique token"
                    );
                    stop_token.stop();
                } else {
                    tracing::warn!(error = ?error, "Telegram update listener error; polling will retry");
                }
            }
        });

        Dispatcher::builder(bot, handler)
            .build()
            .dispatch_with_listener(listener, listener_error_handler)
            .await;

        if polling_conflict.load(Ordering::Acquire) {
            anyhow::bail!(
                "another Telegram getUpdates consumer is using this bot token; only one polling instance may run"
            );
        }
        Ok(())
    }
}

fn is_polling_conflict(error: &RequestError) -> bool {
    matches!(
        error,
        RequestError::Api(ApiError::TerminatedByOtherGetUpdates)
    )
}

async fn queue_album(
    bot: Bot,
    state: Arc<AppState>,
    runtime: Arc<TelegramRuntime>,
    msg: teloxide::types::Message,
    group_id: String,
) {
    let key = (msg.chat.id.0, group_id);
    let generation = {
        let mut albums = runtime.albums.lock().await;
        if !albums.contains_key(&key) && !try_activate_chat(&runtime, msg.chat.id.0).await {
            drop(albums);
            let _ = send_reply(
                &bot,
                msg.chat.id,
                Some(msg.id),
                "I'm still working on your previous message. Use /cancel if you want me to stop.",
                None,
            )
            .await;
            return;
        }
        let album = albums.entry(key.clone()).or_insert(PendingAlbum {
            generation: 0,
            messages: Vec::new(),
        });
        album.generation += 1;
        if album.messages.len() < 10 {
            album.messages.push(msg);
        }
        album.generation
    };

    tokio::spawn(async move {
        tokio::time::sleep(MEDIA_GROUP_DEBOUNCE).await;
        let messages = {
            let mut albums = runtime.albums.lock().await;
            let Some(album) = albums.get(&key) else {
                return;
            };
            if album.generation != generation {
                return;
            }
            albums.remove(&key).map(|album| album.messages)
        };
        if let Some(messages) = messages {
            let chat_id = key.0;
            process_turn(bot, state, messages).await;
            release_chat(&runtime, chat_id).await;
        }
    });
}

async fn handle_command(
    bot: Bot,
    state: Arc<AppState>,
    runtime: Arc<TelegramRuntime>,
    msg: teloxide::types::Message,
    command: Command,
) {
    match command {
        Command::Start | Command::Help => {
            let text = "Send me a message, photo, document, voice note, or audio file.\n\n/new — start a fresh conversation\n/cancel — stop the current run\n/help — show this message";
            let _ = send_reply(&bot, msg.chat.id, Some(msg.id), text, None).await;
        }
        Command::New => {
            if chat_is_active(&runtime, msg.chat.id.0).await {
                let _ = send_reply(
                    &bot,
                    msg.chat.id,
                    Some(msg.id),
                    "I'm still working. Use /cancel first, then /new.",
                    None,
                )
                .await;
                return;
            }
            match state.db.create_conversation(Some("Telegram chat")).await {
                Ok(conversation) => {
                    if let Err(error) = state
                        .db
                        .link_telegram_conversation(msg.chat.id.0, conversation.id)
                        .await
                    {
                        tracing::error!(
                            chat_id = msg.chat.id.0,
                            "Failed to link Telegram conversation: {error}"
                        );
                        let _ = send_generic_error(&bot, &msg).await;
                    } else {
                        let _ = send_reply(
                            &bot,
                            msg.chat.id,
                            Some(msg.id),
                            "Started a fresh conversation.",
                            None,
                        )
                        .await;
                    }
                }
                Err(error) => {
                    tracing::error!(
                        chat_id = msg.chat.id.0,
                        "Failed to create Telegram conversation: {error}"
                    );
                    let _ = send_generic_error(&bot, &msg).await;
                }
            }
        }
        Command::Cancel => match state.db.get_telegram_conversation(msg.chat.id.0).await {
            Ok(Some(conversation_id)) if chat_is_active(&runtime, msg.chat.id.0).await => {
                state.request_cancel(conversation_id).await;
                let _ = send_reply(
                    &bot,
                    msg.chat.id,
                    Some(msg.id),
                    "Stop requested. I'll finish the current network operation, then stop.",
                    None,
                )
                .await;
            }
            Ok(_) => {
                let _ = send_reply(
                    &bot,
                    msg.chat.id,
                    Some(msg.id),
                    "There isn't an active run to stop.",
                    None,
                )
                .await;
            }
            Err(error) => {
                tracing::error!(
                    chat_id = msg.chat.id.0,
                    "Failed to inspect Telegram conversation: {error}"
                );
                let _ = send_generic_error(&bot, &msg).await;
            }
        },
    }
}

async fn process_turn(bot: Bot, state: Arc<AppState>, messages: Vec<teloxide::types::Message>) {
    let Some(first) = messages.first() else {
        return;
    };
    let chat_id = first.chat.id;
    let reply_to = first.id;
    let typing = spawn_typing(bot.clone(), chat_id);
    let result = process_turn_inner(&bot, &state, &messages).await;
    let _ = typing.send(());
    match result {
        Ok(TurnResult::Reply { text, keyboard }) => {
            if let Err(error) = send_reply(&bot, chat_id, Some(reply_to), &text, keyboard).await {
                tracing::error!(
                    chat_id = chat_id.0,
                    "Failed to send Telegram response: {error}"
                );
            }
        }
        Err(error) => {
            tracing::error!(chat_id = chat_id.0, "Telegram turn failed: {error:#}");
            let _ = send_reply(
                &bot,
                chat_id,
                Some(reply_to),
                user_facing_error(&error),
                None,
            )
            .await;
        }
    }
}

enum TurnResult {
    Reply {
        text: String,
        keyboard: Option<InlineKeyboardMarkup>,
    },
}

async fn process_turn_inner(
    bot: &Bot,
    state: &Arc<AppState>,
    telegram_messages: &[teloxide::types::Message],
) -> anyhow::Result<TurnResult> {
    let chat_id = telegram_messages[0].chat.id.0;
    let conversation_id = get_or_create_conversation(state, chat_id).await?;

    let pending = pending_actions(state, conversation_id).await?;
    if !pending.is_empty() {
        let text = telegram_messages
            .iter()
            .find_map(|message| message.text())
            .unwrap_or_default();
        let Some(decision) = pending_reply(text, pending.len()) else {
            return Ok(TurnResult::Reply {
                text: "This conversation is waiting for an action decision. Approve or reject it before sending another request.".to_string(),
                keyboard: Some(pending_keyboard(&pending)),
            });
        };
        let approve = matches!(decision, PendingReply::Approve);
        let mut should_resume = false;
        for action in pending {
            let outcome = jossie_server::handlers::actions::decide_action_deferred(
                state.clone(),
                action.id,
                approve,
            )
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            should_resume |= outcome.batch_resolved;
        }
        if !should_resume {
            return Ok(TurnResult::Reply {
                text: "Decision recorded. Other actions still need a decision.".to_string(),
                keyboard: None,
            });
        }
        let response = jossie_server::run_agent_loop(state, conversation_id).await?;
        let remaining = pending_actions(state, conversation_id).await?;
        return Ok(TurnResult::Reply {
            text: approval_text(response, &remaining),
            keyboard: (!remaining.is_empty()).then(|| pending_keyboard(&remaining)),
        });
    }

    let local_media = download_media_group(bot, state, telegram_messages).await?;
    let content = match build_user_content(state, telegram_messages, &local_media).await {
        Ok(content) => content,
        Err(error) => {
            cleanup_local_media(state, &local_media).await;
            return Err(error);
        }
    };
    if content.trim().is_empty() && local_media.is_empty() {
        return Ok(TurnResult::Reply {
            text: "Send me text, a photo, a supported document, a voice note, or an audio file."
                .to_string(),
            keyboard: None,
        });
    }

    let attachments = local_media
        .iter()
        .map(|media| Attachment {
            id: media.id,
            name: media.name.clone(),
            mime_type: Some(media.mime_type.clone()),
            size: media.size as i64,
            data: None,
        })
        .collect::<Vec<_>>();
    let mut user_message = JossieMessage::new(conversation_id, Role::User, content);
    if !attachments.is_empty() {
        user_message = user_message.with_attachments(attachments);
    }

    if let Err(error) = persist_media_message(state, &user_message, &local_media).await {
        cleanup_local_media(state, &local_media).await;
        return Err(error);
    }
    let response = jossie_server::run_agent_loop(state, conversation_id).await?;
    let pending = pending_actions(state, conversation_id).await?;
    Ok(TurnResult::Reply {
        text: approval_text(response, &pending),
        keyboard: (!pending.is_empty()).then(|| pending_keyboard(&pending)),
    })
}

async fn get_or_create_conversation(state: &AppState, chat_id: i64) -> anyhow::Result<Uuid> {
    if let Some(id) = state.db.get_telegram_conversation(chat_id).await? {
        return Ok(id);
    }
    let conversation = state
        .db
        .create_conversation(Some(&format!("Telegram chat {chat_id}")))
        .await?;
    state
        .db
        .link_telegram_conversation(chat_id, conversation.id)
        .await?;
    Ok(conversation.id)
}

async fn pending_actions(
    state: &AppState,
    conversation_id: Uuid,
) -> anyhow::Result<Vec<jossie_db::PendingAction>> {
    Ok(state
        .db
        .list_pending_actions(Some(conversation_id))
        .await?
        .into_iter()
        .filter(|action| action.status == "pending")
        .collect())
}

fn pending_keyboard(actions: &[jossie_db::PendingAction]) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(actions.iter().map(|action| {
        vec![
            InlineKeyboardButton::callback(
                format!("Approve: {}", action.title),
                format!("pa:y:{}", action.id),
            ),
            InlineKeyboardButton::callback("Reject", format!("pa:n:{}", action.id)),
        ]
    }))
}

fn approval_text(response: String, actions: &[jossie_db::PendingAction]) -> String {
    if actions.is_empty() {
        return response;
    }
    let details = actions
        .iter()
        .map(|action| format!("- {}: {}", action.title, action.summary))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{response}\n\nPending actions:\n{details}")
}

async fn handle_callback(
    bot: Bot,
    state: Arc<AppState>,
    runtime: Arc<TelegramRuntime>,
    query: CallbackQuery,
) {
    let Some(data) = query.data.as_deref() else {
        let _ = bot.answer_callback_query(query.id).await;
        return;
    };
    let mut parts = data.splitn(3, ':');
    if parts.next() != Some("pa") {
        let _ = bot.answer_callback_query(query.id).await;
        return;
    }
    let approve = match parts.next() {
        Some("y") => true,
        Some("n") => false,
        _ => {
            let _ = bot.answer_callback_query(query.id).await;
            return;
        }
    };
    let Some(action_id) = parts.next() else {
        let _ = bot.answer_callback_query(query.id).await;
        return;
    };
    let Some(origin) = query.message.as_ref() else {
        let _ = bot.answer_callback_query(query.id).await;
        return;
    };
    let chat_id = origin.chat().id;
    let message_id = origin.id();
    if !origin.chat().is_private() {
        let _ = bot.answer_callback_query(query.id).await;
        return;
    }
    if !try_activate_chat(&runtime, chat_id.0).await {
        let _ = bot.answer_callback_query(query.id).await;
        return;
    }
    let typing = spawn_typing(bot.clone(), chat_id);
    let result = jossie_server::handlers::actions::decide_action_deferred(
        state.clone(),
        action_id.to_string(),
        approve,
    )
    .await;
    let _ = bot.answer_callback_query(query.id).await;
    match result {
        Ok(outcome) => {
            let remaining = pending_actions(&state, outcome.conversation_id)
                .await
                .unwrap_or_default();
            let edit = bot.edit_message_reply_markup(chat_id, message_id);
            if remaining.is_empty() {
                let _ = edit.await;
            } else {
                let _ = edit.reply_markup(pending_keyboard(&remaining)).await;
            }
            if outcome.batch_resolved {
                match jossie_server::run_agent_loop(&state, outcome.conversation_id).await {
                    Ok(response) => {
                        let pending = pending_actions(&state, outcome.conversation_id)
                            .await
                            .unwrap_or_default();
                        let _ = typing.send(());
                        let _ = send_reply(
                            &bot,
                            chat_id,
                            Some(message_id),
                            &approval_text(response, &pending),
                            (!pending.is_empty()).then(|| pending_keyboard(&pending)),
                        )
                        .await;
                    }
                    Err(error) => {
                        let _ = typing.send(());
                        tracing::error!(
                            chat_id = chat_id.0,
                            "Failed to resume approved Telegram run: {error}"
                        );
                        let _ = bot
                            .send_message(
                                chat_id,
                                "I couldn't continue that run. Please try again.",
                            )
                            .await;
                    }
                }
            } else {
                let _ = typing.send(());
            }
        }
        Err(error) => {
            let _ = typing.send(());
            tracing::error!(
                chat_id = chat_id.0,
                "Telegram action decision failed: {error}"
            );
            let _ = bot
                .send_message(
                    chat_id,
                    "That action could not be updated. It may already be resolved.",
                )
                .await;
        }
    }
    release_chat(&runtime, chat_id.0).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MediaKind {
    ModelAttachment,
    Voice,
    Audio,
}

struct LocalMedia {
    id: Uuid,
    name: String,
    mime_type: String,
    size: usize,
    path: PathBuf,
    kind: MediaKind,
}

struct MediaCandidate {
    file_id: FileId,
    name: String,
    mime_type: String,
    size: usize,
    kind: MediaKind,
}

async fn download_media_group(
    bot: &Bot,
    state: &AppState,
    messages: &[teloxide::types::Message],
) -> anyhow::Result<Vec<LocalMedia>> {
    let mut candidates = Vec::new();
    for message in messages {
        if let Some(photos) = message.photo()
            && let Some(photo) = photos.iter().max_by_key(|photo| photo.width * photo.height)
        {
            candidates.push(MediaCandidate {
                file_id: photo.file.id.clone(),
                name: format!("telegram-photo-{}.jpg", message.id.0),
                mime_type: "image/jpeg".to_string(),
                size: photo.file.size as usize,
                kind: MediaKind::ModelAttachment,
            });
            continue;
        }
        if let Some(document) = message.document() {
            let name = document
                .file_name
                .clone()
                .unwrap_or_else(|| format!("telegram-document-{}", message.id.0));
            let mime_type = document
                .mime_type
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "application/octet-stream".to_string());
            if !supported_document(&name, &mime_type) {
                anyhow::bail!("Unsupported Telegram document: {name}");
            }
            candidates.push(MediaCandidate {
                file_id: document.file.id.clone(),
                name,
                mime_type,
                size: document.file.size as usize,
                kind: MediaKind::ModelAttachment,
            });
            continue;
        }
        if let Some(voice) = message.voice() {
            ensure_voice_available(state).await?;
            candidates.push(MediaCandidate {
                file_id: voice.file.id.clone(),
                name: format!("telegram-voice-{}.ogg", message.id.0),
                mime_type: voice
                    .mime_type
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "audio/ogg".to_string()),
                size: voice.file.size as usize,
                kind: MediaKind::Voice,
            });
            continue;
        }
        if let Some(audio) = message.audio() {
            ensure_transcription_enabled(state)?;
            candidates.push(MediaCandidate {
                file_id: audio.file.id.clone(),
                name: audio
                    .file_name
                    .clone()
                    .unwrap_or_else(|| format!("telegram-audio-{}", message.id.0)),
                mime_type: audio
                    .mime_type
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "audio/mpeg".to_string()),
                size: audio.file.size as usize,
                kind: MediaKind::Audio,
            });
        }
    }
    let total_size = candidates
        .iter()
        .map(|candidate| candidate.size)
        .sum::<usize>();
    if total_size > state.telegram_max_download_bytes {
        anyhow::bail!(
            "Telegram media is too large: {total_size} bytes exceeds {}",
            state.telegram_max_download_bytes
        );
    }
    tokio::fs::create_dir_all("uploads").await?;
    let mut downloaded = Vec::new();
    for candidate in candidates {
        match download_candidate(bot, state, candidate).await {
            Ok(media) => {
                let total = downloaded
                    .iter()
                    .map(|item: &LocalMedia| item.size)
                    .sum::<usize>()
                    + media.size;
                if total > state.telegram_max_download_bytes {
                    let _ = tokio::fs::remove_file(&media.path).await;
                    cleanup_paths(
                        downloaded
                            .iter()
                            .map(|item: &LocalMedia| item.path.as_path()),
                    )
                    .await;
                    anyhow::bail!("Downloaded Telegram album exceeds the configured limit");
                }
                downloaded.push(media);
            }
            Err(error) => {
                cleanup_paths(
                    downloaded
                        .iter()
                        .map(|media: &LocalMedia| media.path.as_path()),
                )
                .await;
                return Err(error);
            }
        }
    }
    Ok(downloaded)
}

async fn download_candidate(
    bot: &Bot,
    state: &AppState,
    candidate: MediaCandidate,
) -> anyhow::Result<LocalMedia> {
    let id = Uuid::new_v4();
    let path = PathBuf::from("uploads").join(id.to_string());
    let result = async {
        let file = bot.get_file(candidate.file_id).await?;
        let mut destination = tokio::fs::File::create(&path).await?;
        bot.download_file(&file.path, &mut destination).await?;
        drop(destination);
        let actual_size = tokio::fs::metadata(&path).await?.len() as usize;
        if actual_size > state.telegram_max_download_bytes {
            anyhow::bail!("Downloaded Telegram file exceeds the configured limit");
        }
        Ok(LocalMedia {
            id,
            name: candidate.name,
            mime_type: candidate.mime_type,
            size: actual_size,
            path: path.clone(),
            kind: candidate.kind,
        })
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
    }
    result
}

async fn build_user_content(
    state: &AppState,
    messages: &[teloxide::types::Message],
    media: &[LocalMedia],
) -> anyhow::Result<String> {
    let caption = messages
        .iter()
        .find_map(|message| message.caption().or_else(|| message.text()))
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut transcripts = Vec::new();
    for item in media
        .iter()
        .filter(|item| matches!(item.kind, MediaKind::Voice | MediaKind::Audio))
    {
        let (path, filename, mime, temporary) =
            prepare_audio_for_transcription(state, item).await?;
        let transcript = state.llm.transcribe_file(&path, &filename, &mime).await;
        if temporary {
            let _ = tokio::fs::remove_file(&path).await;
        }
        transcripts.push(transcript?);
    }
    if !transcripts.is_empty() {
        let transcript = transcripts.join("\n\n");
        return Ok(if caption.is_empty() {
            transcript
        } else {
            format!("{caption}\n\nVoice transcript:\n{transcript}")
        });
    }
    if !caption.is_empty() {
        return Ok(caption);
    }
    if media
        .iter()
        .any(|item| item.mime_type.starts_with("image/"))
    {
        Ok("Please inspect the attached image or images and respond appropriately.".to_string())
    } else if !media.is_empty() {
        Ok("Please inspect and briefly summarize the attached document or documents.".to_string())
    } else {
        Ok(String::new())
    }
}

async fn prepare_audio_for_transcription(
    state: &AppState,
    media: &LocalMedia,
) -> anyhow::Result<(PathBuf, String, String, bool)> {
    let extension = Path::new(&media.name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "mp3" | "mp4" | "mpeg" | "mpga" | "m4a" | "wav" | "webm"
    ) {
        return Ok((
            media.path.clone(),
            media.name.clone(),
            media.mime_type.clone(),
            false,
        ));
    }
    let output = PathBuf::from("uploads").join(format!("transcode-{}.webm", Uuid::new_v4()));
    let status = tokio::process::Command::new(&state.telegram_ffmpeg_path)
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(&media.path)
        .args(["-c:a", "libopus", "-b:a", "64k"])
        .arg(&output)
        .status()
        .await?;
    if !status.success() {
        let _ = tokio::fs::remove_file(&output).await;
        anyhow::bail!("FFmpeg could not transcode the Telegram audio file");
    }
    Ok((
        output,
        format!("{}.webm", media.id),
        "audio/webm".to_string(),
        true,
    ))
}

async fn ensure_voice_available(state: &AppState) -> anyhow::Result<()> {
    ensure_transcription_enabled(state)?;
    let output = tokio::process::Command::new(&state.telegram_ffmpeg_path)
        .arg("-version")
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!("FFmpeg is unavailable");
    }
    Ok(())
}

fn ensure_transcription_enabled(state: &AppState) -> anyhow::Result<()> {
    if !state.llm.transcription_is_configured() {
        anyhow::bail!("Voice transcription is disabled");
    }
    Ok(())
}

async fn persist_media_message(
    state: &AppState,
    message: &JossieMessage,
    media: &[LocalMedia],
) -> anyhow::Result<()> {
    let mut saved = Vec::new();
    for item in media {
        if let Err(error) = state
            .db
            .save_file_record(
                &item.id,
                &item.name,
                Some(&item.mime_type),
                item.size as i64,
                item.path.to_string_lossy().as_ref(),
                Some(message.conversation_id),
            )
            .await
        {
            for id in saved {
                let _ = state.db.delete_file_record(&id).await;
            }
            return Err(error);
        }
        saved.push(item.id);
    }
    jossie_server::events::persist_message(state, message).await?;
    for item in media {
        state
            .db
            .link_message_attachment(message.id, item.id)
            .await?;
    }
    Ok(())
}

async fn cleanup_local_media(state: &AppState, media: &[LocalMedia]) {
    for item in media {
        let _ = state.db.delete_file_record(&item.id).await;
        let _ = tokio::fs::remove_file(&item.path).await;
    }
}

async fn cleanup_paths<'a>(paths: impl Iterator<Item = &'a Path>) {
    for path in paths {
        let _ = tokio::fs::remove_file(path).await;
    }
}

fn supported_document(name: &str, mime: &str) -> bool {
    if matches!(
        mime,
        "application/pdf" | "image/jpeg" | "image/png" | "image/webp" | "image/gif"
    ) || mime.starts_with("text/")
    {
        return true;
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "pdf"
            | "jpg"
            | "jpeg"
            | "png"
            | "webp"
            | "gif"
            | "txt"
            | "md"
            | "json"
            | "html"
            | "xml"
            | "yaml"
            | "yml"
            | "csv"
            | "tsv"
            | "doc"
            | "docx"
            | "rtf"
            | "odt"
            | "ppt"
            | "pptx"
            | "xls"
            | "xlsx"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "java"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "go"
            | "rb"
            | "php"
            | "sh"
            | "sql"
            | "toml"
            | "css"
    )
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn identifies_competing_get_updates_as_a_terminal_polling_conflict() {
        assert!(is_polling_conflict(&RequestError::Api(
            ApiError::TerminatedByOtherGetUpdates
        )));
        assert!(!is_polling_conflict(&RequestError::Api(
            ApiError::InvalidToken
        )));
    }

    #[test]
    fn split_message_uses_character_limit_and_word_boundaries() {
        let text = format!("{} {}", "😀".repeat(4090), "hello world");
        let chunks = split_message(&text, TELEGRAM_MESSAGE_LIMIT);
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= TELEGRAM_MESSAGE_LIMIT)
        );
        assert_eq!(chunks.join(" "), text);
    }

    #[test]
    fn supported_documents_reject_archives_and_executables() {
        assert!(supported_document("report.pdf", "application/pdf"));
        assert!(supported_document("notes.md", "text/markdown"));
        assert!(!supported_document("archive.zip", "application/zip"));
        assert!(!supported_document(
            "program.exe",
            "application/octet-stream"
        ));
    }

    #[test]
    fn callback_data_stays_within_telegram_limit() {
        let id = Uuid::new_v4().to_string();
        assert!(format!("pa:y:{id}").len() <= 64);
        assert!(format!("pa:n:{id}").len() <= 64);
    }

    #[tokio::test]
    async fn typing_status_is_sent_immediately_and_refreshed_until_stopped() {
        use axum::{Json, Router, extract::State};
        use serde_json::json;

        async fn record_action(State(calls): State<Arc<AtomicUsize>>) -> Json<serde_json::Value> {
            calls.fetch_add(1, Ordering::SeqCst);
            Json(json!({"ok": true, "result": true}))
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .fallback(record_action)
            .with_state(calls.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let bot = Bot::new("TEST").set_api_url(format!("http://{address}/").parse().unwrap());
        let stop = spawn_typing_with_interval(bot, ChatId(42), Duration::from_millis(15));
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = stop.send(());
        tokio::time::sleep(Duration::from_millis(10)).await;
        let stopped_at = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(35)).await;

        assert!(stopped_at >= 2);
        assert_eq!(calls.load(Ordering::SeqCst), stopped_at);
    }
}
