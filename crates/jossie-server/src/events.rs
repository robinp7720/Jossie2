use crate::state::AppState;
use jossie_core::types::Message;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    RunStarted {
        conversation_id: Uuid,
        run_id: String,
        scheduled: bool,
    },
    AssistantThinking {
        conversation_id: Uuid,
        run_id: String,
        iteration: usize,
    },
    AssistantDelta {
        conversation_id: Uuid,
        run_id: String,
        content: String,
    },
    AssistantReset {
        conversation_id: Uuid,
        run_id: String,
        reason: String,
    },
    ToolCalled {
        conversation_id: Uuid,
        run_id: String,
        call_id: String,
        tool: String,
        arguments_preview: String,
    },
    ToolStarted {
        conversation_id: Uuid,
        run_id: String,
        call_id: String,
        tool: String,
    },
    ToolFinished {
        conversation_id: Uuid,
        run_id: String,
        call_id: String,
        tool: String,
        result_preview: String,
        is_error: bool,
    },
    ReflectionRetry {
        conversation_id: Uuid,
        run_id: String,
        feedback: String,
    },
    RunCompleted {
        conversation_id: Uuid,
        run_id: String,
    },
    RunCancelled {
        conversation_id: Uuid,
        run_id: String,
    },
    CancelRequested {
        conversation_id: Uuid,
    },
    MessageCreated {
        conversation_id: Uuid,
        message: Message,
    },
    ConversationUpdated {
        conversation_id: Uuid,
        title: Option<String>,
        updated_at: String,
    },
    BackgroundNotification {
        conversation_id: Uuid,
        source: String,
        message: String,
    },
    Error {
        conversation_id: Uuid,
        run_id: Option<String>,
        error: String,
    },
}

impl ServerEvent {
    pub fn conversation_id(&self) -> Uuid {
        match self {
            ServerEvent::RunStarted {
                conversation_id, ..
            }
            | ServerEvent::AssistantThinking {
                conversation_id, ..
            }
            | ServerEvent::AssistantDelta {
                conversation_id, ..
            }
            | ServerEvent::AssistantReset {
                conversation_id, ..
            }
            | ServerEvent::ToolCalled {
                conversation_id, ..
            }
            | ServerEvent::ToolStarted {
                conversation_id, ..
            }
            | ServerEvent::ToolFinished {
                conversation_id, ..
            }
            | ServerEvent::ReflectionRetry {
                conversation_id, ..
            }
            | ServerEvent::RunCompleted {
                conversation_id, ..
            }
            | ServerEvent::RunCancelled {
                conversation_id, ..
            }
            | ServerEvent::CancelRequested { conversation_id }
            | ServerEvent::MessageCreated {
                conversation_id, ..
            }
            | ServerEvent::ConversationUpdated {
                conversation_id, ..
            }
            | ServerEvent::BackgroundNotification {
                conversation_id, ..
            }
            | ServerEvent::Error {
                conversation_id, ..
            } => *conversation_id,
        }
    }
}

pub fn preview_text(content: &str, max_len: usize) -> String {
    let trimmed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = trimmed.chars();
    let preview = chars.by_ref().take(max_len).collect::<String>();
    if chars.next().is_none() {
        trimmed
    } else {
        format!("{preview}...")
    }
}

/// Persist a compact, owner-visible activity record without leaking private
/// prompts, chain-of-thought, raw tool arguments, or raw tool output.
pub async fn persist_activity_event(db: &jossie_db::Database, event: &ServerEvent) {
    let summary = match event {
        ServerEvent::RunStarted {
            conversation_id,
            run_id,
            scheduled,
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "run",
            if *scheduled {
                "Started scheduled work"
            } else {
                "Started a conversation"
            },
            None,
            "normal",
        )),
        ServerEvent::ToolCalled {
            conversation_id,
            run_id,
            tool,
            ..
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "tool",
            "Used a capability",
            Some(tool.as_str()),
            "normal",
        )),
        ServerEvent::ToolFinished {
            conversation_id,
            run_id,
            tool,
            is_error,
            ..
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "tool",
            if *is_error {
                "A capability needs attention"
            } else {
                "Completed a capability"
            },
            Some(tool.as_str()),
            if *is_error { "warn" } else { "success" },
        )),
        ServerEvent::ReflectionRetry {
            conversation_id,
            run_id,
            ..
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "reflection",
            "Refined a response",
            None,
            "normal",
        )),
        ServerEvent::RunCompleted {
            conversation_id,
            run_id,
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "run",
            "Finished a conversation",
            None,
            "success",
        )),
        ServerEvent::RunCancelled {
            conversation_id,
            run_id,
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "run",
            "Cancelled a conversation",
            None,
            "warn",
        )),
        ServerEvent::CancelRequested { conversation_id } => Some((
            Some(*conversation_id),
            None,
            "run",
            "Cancellation requested",
            None,
            "warn",
        )),
        ServerEvent::BackgroundNotification {
            conversation_id,
            source,
            ..
        } => Some((
            Some(*conversation_id),
            None,
            "background",
            "New background update",
            Some(source.as_str()),
            "success",
        )),
        ServerEvent::Error {
            conversation_id,
            run_id,
            ..
        } => Some((
            Some(*conversation_id),
            run_id.as_deref(),
            "error",
            "A run needs attention",
            None,
            "warn",
        )),
        _ => None,
    };

    if let Some((conversation_id, run_id, category, title, detail, tone)) = summary {
        if let Err(error) = db
            .record_activity_event(conversation_id, run_id, category, title, detail, tone)
            .await
        {
            tracing::warn!("Failed to persist dashboard activity: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::preview_text;

    #[test]
    fn preview_text_truncates_on_char_boundaries() {
        let content = "If either wasn’t you, check those accounts now.";

        assert_eq!(preview_text(content, 16), "If either wasn’t...");
    }

    #[test]
    fn preview_text_preserves_short_unicode_content() {
        let content = "PayPal says you sent €7.50.";

        assert_eq!(preview_text(content, 100), content);
    }
}

pub async fn persist_message(state: &AppState, message: &Message) -> anyhow::Result<()> {
    state.db.save_message(message).await?;
    state.publish_event(ServerEvent::MessageCreated {
        conversation_id: message.conversation_id,
        message: message.clone(),
    });

    if let Some(conversation) = state.db.get_conversation(message.conversation_id).await? {
        state.publish_event(ServerEvent::ConversationUpdated {
            conversation_id: conversation.id,
            title: conversation.title,
            updated_at: conversation.updated_at.to_rfc3339(),
        });
    }

    Ok(())
}
