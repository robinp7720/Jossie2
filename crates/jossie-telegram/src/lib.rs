use jossie_core::types::{Attachment, Message as JossieMessage, Role};
use jossie_server::AppState;
use jossie_server::events::ServerEvent;
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

include!("runtime.rs");
include!("goal_notifications.rs");
include!("commands.rs");
include!("turns.rs");
include!("media.rs");
include!("delivery.rs");
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    include!("tests.rs");
}
