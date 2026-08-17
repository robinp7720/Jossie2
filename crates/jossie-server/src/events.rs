use crate::state::AppState;
use jossie_core::types::Message;
use jossie_db::WorkRunStatus;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
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
    CapabilitiesActivated {
        conversation_id: Uuid,
        run_id: String,
        capabilities: Vec<String>,
    },
    ActionApprovalRequired {
        conversation_id: Uuid,
        run_id: String,
        action: jossie_db::PendingAction,
    },
    ActionResolved {
        conversation_id: Uuid,
        run_id: String,
        action_id: String,
        status: String,
        title: String,
    },
    RunWaitingForApproval {
        conversation_id: Uuid,
        run_id: String,
        batch_id: String,
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
    RunPaused {
        conversation_id: Uuid,
        run_id: String,
        goal_id: String,
        reason: String,
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
        archived_at: Option<String>,
        updated_at: String,
    },
    ConversationDeleted {
        conversation_id: Uuid,
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
    GoalUpdated {
        conversation_id: Uuid,
        goal: jossie_db::GoalWithTasks,
    },
    WorkRunUpdated {
        conversation_id: Option<Uuid>,
        run: jossie_db::WorkRun,
    },
    WorkStepUpdated {
        conversation_id: Option<Uuid>,
        run_id: String,
        step: jossie_db::WorkRunStep,
    },
    WorkerStatusUpdated {
        worker: jossie_db::WorkerStatus,
    },
}

impl ServerEvent {
    pub fn conversation_id(&self) -> Option<Uuid> {
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
            | ServerEvent::CapabilitiesActivated {
                conversation_id, ..
            }
            | ServerEvent::ActionApprovalRequired {
                conversation_id, ..
            }
            | ServerEvent::ActionResolved {
                conversation_id, ..
            }
            | ServerEvent::RunWaitingForApproval {
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
            | ServerEvent::RunPaused {
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
            | ServerEvent::ConversationDeleted { conversation_id }
            | ServerEvent::BackgroundNotification {
                conversation_id, ..
            }
            | ServerEvent::Error {
                conversation_id, ..
            }
            | ServerEvent::GoalUpdated {
                conversation_id, ..
            } => Some(*conversation_id),
            ServerEvent::WorkRunUpdated {
                conversation_id, ..
            }
            | ServerEvent::WorkStepUpdated {
                conversation_id, ..
            } => *conversation_id,
            ServerEvent::WorkerStatusUpdated { .. } => None,
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

/// Convert the execution event stream into durable, privacy-safe work state.
/// Raw arguments, model reasoning, and raw tool results are intentionally not stored.
pub async fn persist_work_event(db: &jossie_db::Database, event: &ServerEvent) -> Vec<ServerEvent> {
    let result: anyhow::Result<Vec<ServerEvent>> = async {
        let mut derived = Vec::new();
        match event {
            ServerEvent::RunStarted {
                conversation_id,
                run_id,
                scheduled,
            } => {
                db.create_work_run(jossie_db::NewWorkRun {
                    id: Some(run_id),
                    goal_id: None,
                    task_id: None,
                    conversation_id: Some(*conversation_id),
                    kind: if *scheduled { "scheduled" } else { "chat" },
                    source_type: None,
                    source_id: None,
                    summary: if *scheduled {
                        "Scheduled work"
                    } else {
                        "Conversation request"
                    },
                    visibility: "significant",
                })
                .await?;
                db.update_work_run(
                    run_id,
                    WorkRunStatus::Running,
                    Some("Planning the next step"),
                    None,
                )
                .await?;
                if let Some(run) = db.get_work_run(run_id).await? {
                    derived.push(ServerEvent::WorkRunUpdated {
                        conversation_id: Some(*conversation_id),
                        run,
                    });
                }
            }
            ServerEvent::AssistantThinking {
                conversation_id,
                run_id,
                iteration,
            } => {
                db.update_work_run(
                    run_id,
                    WorkRunStatus::Running,
                    Some(if *iteration == 0 {
                        "Understanding the request"
                    } else {
                        "Considering the next step"
                    }),
                    None,
                )
                .await?;
                if let Some(run) = db.get_work_run(run_id).await? {
                    derived.push(ServerEvent::WorkRunUpdated {
                        conversation_id: Some(*conversation_id),
                        run,
                    });
                }
            }
            ServerEvent::CapabilitiesActivated {
                conversation_id,
                run_id,
                capabilities,
            } => {
                let label = if capabilities.is_empty() {
                    "Prepared capabilities".to_string()
                } else {
                    format!("Prepared {}", capabilities.join(", "))
                };
                let step = db
                    .complete_instant_work_run_step(run_id, "capability", &label, None)
                    .await?;
                derived.push(ServerEvent::WorkStepUpdated {
                    conversation_id: Some(*conversation_id),
                    run_id: run_id.clone(),
                    step,
                });
            }
            ServerEvent::ToolStarted {
                conversation_id,
                run_id,
                call_id,
                tool,
            } => {
                let label = format!("Using {}", tool.replace('_', " "));
                db.update_work_run(run_id, WorkRunStatus::Running, Some(&label), None)
                    .await?;
                let step = db
                    .create_work_run_step(run_id, Some(call_id), "capability", &label)
                    .await?;
                derived.push(ServerEvent::WorkStepUpdated {
                    conversation_id: Some(*conversation_id),
                    run_id: run_id.clone(),
                    step,
                });
            }
            ServerEvent::ToolFinished {
                conversation_id,
                run_id,
                call_id,
                tool,
                is_error,
                ..
            } => {
                let status = if *is_error { "failed" } else { "completed" };
                let summary = if *is_error {
                    Some("Capability reported an error")
                } else {
                    Some("Capability completed")
                };
                db.finish_work_run_step(
                    call_id,
                    status,
                    summary,
                    if *is_error {
                        Some("Capability needs attention")
                    } else {
                        None
                    },
                )
                .await?;
                if let Some(step) = db
                    .list_work_run_steps(run_id)
                    .await?
                    .into_iter()
                    .find(|step| step.id == *call_id)
                {
                    derived.push(ServerEvent::WorkStepUpdated {
                        conversation_id: Some(*conversation_id),
                        run_id: run_id.clone(),
                        step,
                    });
                }
                db.update_work_run(
                    run_id,
                    WorkRunStatus::Running,
                    Some(&format!("Finished using {}", tool.replace('_', " "))),
                    None,
                )
                .await?;
            }
            ServerEvent::ReflectionRetry {
                conversation_id,
                run_id,
                ..
            } => {
                let step = db
                    .complete_instant_work_run_step(
                        run_id,
                        "reflection",
                        "Refined the response",
                        None,
                    )
                    .await?;
                derived.push(ServerEvent::WorkStepUpdated {
                    conversation_id: Some(*conversation_id),
                    run_id: run_id.clone(),
                    step,
                });
            }
            ServerEvent::ActionApprovalRequired {
                conversation_id,
                run_id,
                action,
            } => {
                db.update_work_run(
                    run_id,
                    WorkRunStatus::WaitingForApproval,
                    Some(&action.title),
                    None,
                )
                .await?;
                if let Some(run) = db.get_work_run(run_id).await? {
                    derived.push(ServerEvent::WorkRunUpdated {
                        conversation_id: Some(*conversation_id),
                        run,
                    });
                }
            }
            ServerEvent::ActionResolved {
                conversation_id,
                run_id,
                action_id,
                status,
                title,
            } => {
                let batch_resolved = if let Some(action) = db.get_pending_action(action_id).await? {
                    db.pending_action_batch_is_resolved(&action.batch_id)
                        .await?
                } else {
                    true
                };
                let phase = match status.as_str() {
                    "completed" => format!("Completed approved action: {title}"),
                    "rejected" => format!("Action declined: {title}"),
                    _ => format!("Action needs attention: {title}"),
                };
                db.update_work_run(
                    run_id,
                    if batch_resolved {
                        WorkRunStatus::Completed
                    } else {
                        WorkRunStatus::WaitingForApproval
                    },
                    Some(&phase),
                    (status == "failed").then_some("Approved action failed"),
                )
                .await?;
                if let Some(run) = db.get_work_run(run_id).await? {
                    derived.push(ServerEvent::WorkRunUpdated {
                        conversation_id: Some(*conversation_id),
                        run,
                    });
                }
            }
            ServerEvent::RunWaitingForApproval {
                conversation_id,
                run_id,
                ..
            } => {
                db.update_work_run(
                    run_id,
                    WorkRunStatus::WaitingForApproval,
                    Some("Waiting for your approval"),
                    None,
                )
                .await?;
                if let Some(run) = db.get_work_run(run_id).await? {
                    derived.push(ServerEvent::WorkRunUpdated {
                        conversation_id: Some(*conversation_id),
                        run,
                    });
                }
            }
            ServerEvent::RunCompleted {
                conversation_id,
                run_id,
            } => {
                db.update_work_run(run_id, WorkRunStatus::Completed, Some("Finished"), None)
                    .await?;
                if let Some(run) = db.get_work_run(run_id).await? {
                    derived.push(ServerEvent::WorkRunUpdated {
                        conversation_id: Some(*conversation_id),
                        run,
                    });
                }
            }
            ServerEvent::RunPaused {
                conversation_id,
                run_id,
                reason,
                ..
            } => {
                db.update_work_run(run_id, WorkRunStatus::Paused, Some(reason), None)
                    .await?;
                if let Some(run) = db.get_work_run(run_id).await? {
                    derived.push(ServerEvent::WorkRunUpdated {
                        conversation_id: Some(*conversation_id),
                        run,
                    });
                }
            }
            ServerEvent::RunCancelled {
                conversation_id,
                run_id,
            } => {
                db.update_work_run(run_id, WorkRunStatus::Cancelled, Some("Cancelled"), None)
                    .await?;
                if let Some(run) = db.get_work_run(run_id).await? {
                    derived.push(ServerEvent::WorkRunUpdated {
                        conversation_id: Some(*conversation_id),
                        run,
                    });
                }
            }
            ServerEvent::Error {
                conversation_id,
                run_id: Some(run_id),
                error,
            } => {
                db.update_work_run(
                    run_id,
                    WorkRunStatus::Failed,
                    Some("Needs attention"),
                    Some(&preview_text(error, 240)),
                )
                .await?;
                if let Some(run) = db.get_work_run(run_id).await? {
                    derived.push(ServerEvent::WorkRunUpdated {
                        conversation_id: Some(*conversation_id),
                        run,
                    });
                }
            }
            _ => {}
        }
        Ok(derived)
    }
    .await;
    match result {
        Ok(events) => events,
        Err(error) => {
            tracing::warn!("Failed to persist work progress: {error}");
            Vec::new()
        }
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
        ServerEvent::CapabilitiesActivated {
            conversation_id,
            run_id,
            capabilities: _,
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "capability",
            "Prepared capabilities",
            None,
            "normal",
        )),
        ServerEvent::ActionApprovalRequired {
            conversation_id,
            run_id,
            action,
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "approval",
            "Waiting for approval",
            Some(action.title.as_str()),
            "warn",
        )),
        ServerEvent::ActionResolved {
            conversation_id,
            run_id,
            status,
            title,
            ..
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "approval",
            if status == "completed" {
                "Approved action completed"
            } else if status == "rejected" {
                "Action declined"
            } else {
                "Action needs attention"
            },
            Some(title.as_str()),
            if status == "completed" {
                "success"
            } else {
                "warn"
            },
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
        ServerEvent::RunPaused {
            conversation_id,
            run_id,
            reason,
            ..
        } => Some((
            Some(*conversation_id),
            Some(run_id.as_str()),
            "work",
            "Work paused with a checkpoint",
            Some(reason.as_str()),
            "normal",
        )),
        _ => None,
    };

    if let Some((conversation_id, run_id, category, title, detail, tone)) = summary
        && let Err(error) = db
            .record_activity_event(conversation_id, run_id, category, title, detail, tone)
            .await
    {
        tracing::warn!("Failed to persist dashboard activity: {error}");
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
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
    state
        .publish_event(ServerEvent::MessageCreated {
            conversation_id: message.conversation_id,
            message: message.clone(),
        })
        .await;

    if let Some(conversation) = state.db.get_conversation(message.conversation_id).await? {
        state
            .publish_event(ServerEvent::ConversationUpdated {
                conversation_id: conversation.id,
                title: conversation.title,
                archived_at: conversation.archived_at.map(|value| value.to_rfc3339()),
                updated_at: conversation.updated_at.to_rfc3339(),
            })
            .await;
    }

    Ok(())
}
