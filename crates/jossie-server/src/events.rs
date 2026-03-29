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
    if trimmed.len() <= max_len {
        trimmed
    } else {
        format!("{}...", &trimmed[..max_len])
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
