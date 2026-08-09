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
        let goal_before = state
            .db
            .get_active_goal_for_conversation(conversation_id)
            .await?;
        let response = jossie_server::run_agent_loop(state, conversation_id).await?;
        let goal_after = goal_after_run(state, conversation_id, goal_before.as_ref()).await?;
        let remaining = pending_actions(state, conversation_id).await?;
        return Ok(TurnResult::Reply {
            text: approval_text(
                with_conversational_goal_update(
                    response,
                    goal_before.as_ref(),
                    goal_after.as_ref(),
                ),
                &remaining,
            ),
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
    let goal_before = state
        .db
        .get_active_goal_for_conversation(conversation_id)
        .await?;
    let response = if let Some(goal) = goal_before.as_ref().filter(|goal| {
        matches!(goal.goal.status.as_str(), "active" | "paused" | "blocked")
            && should_continue_tracked_goal(goal, &user_message.content, !local_media.is_empty())
    }) {
        continue_tracked_goal(state, conversation_id, goal, goal.goal.status == "paused").await?
    } else {
        let response = jossie_server::run_agent_loop(state, conversation_id).await?;
        let goal_after = goal_after_run(state, conversation_id, goal_before.as_ref()).await?;
        with_conversational_goal_update(response, goal_before.as_ref(), goal_after.as_ref())
    };
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

async fn goal_after_run(
    state: &AppState,
    conversation_id: Uuid,
    before: Option<&jossie_db::GoalWithTasks>,
) -> anyhow::Result<Option<jossie_db::GoalWithTasks>> {
    if let Some(active) = state
        .db
        .get_active_goal_for_conversation(conversation_id)
        .await?
    {
        return Ok(Some(active));
    }
    match before {
        Some(goal) => state.db.get_goal_with_tasks(&goal.goal.id).await,
        None => Ok(None),
    }
}

fn with_conversational_goal_update(
    response: String,
    before: Option<&jossie_db::GoalWithTasks>,
    after: Option<&jossie_db::GoalWithTasks>,
) -> String {
    if !goal_state_changed(before, after) {
        return response;
    }
    let Some(after) = after else {
        return response;
    };
    let status = conversational_goal_status(Some(after));
    if response.trim().is_empty() {
        status
    } else {
        format!("{}\n\n{}", response.trim_end(), status)
    }
}

fn goal_state_changed(
    before: Option<&jossie_db::GoalWithTasks>,
    after: Option<&jossie_db::GoalWithTasks>,
) -> bool {
    match (before, after) {
        (None, Some(_)) | (Some(_), None) => true,
        (None, None) => false,
        (Some(before), Some(after)) => {
            before.goal.id != after.goal.id
                || before.goal.status != after.goal.status
                || before.goal.blocker != after.goal.blocker
                || before.completed_tasks != after.completed_tasks
                || before.total_tasks != after.total_tasks
                || before
                    .tasks
                    .iter()
                    .map(|task| (&task.id, &task.status, &task.blocker))
                    .ne(after
                        .tasks
                        .iter()
                        .map(|task| (&task.id, &task.status, &task.blocker)))
        }
    }
}

fn conversational_goal_status(goal: Option<&jossie_db::GoalWithTasks>) -> String {
    let Some(goal) = goal else {
        return "I don't have any ongoing work at the moment.".to_string();
    };
    let title = &goal.goal.title;
    let progress = if goal.total_tasks == 0 {
        String::new()
    } else {
        format!(
            " I've finished {} of {} parts.",
            goal.completed_tasks, goal.total_tasks
        )
    };
    let next = goal
        .tasks
        .iter()
        .find(|task| {
            matches!(
                task.status.as_str(),
                "in_progress" | "waiting" | "blocked" | "pending"
            )
        })
        .map(|task| task.title.as_str());
    match goal.goal.status.as_str() {
        "blocked" => {
            let blocker = goal
                .goal
                .blocker
                .as_deref()
                .or_else(|| {
                    goal.tasks
                        .iter()
                        .find(|task| task.status == "blocked")
                        .and_then(|task| task.blocker.as_deref())
                })
                .unwrap_or("I need one more piece of information before I can continue");
            format!(
                "I've kept our place on “{title}”.{progress} I'm waiting on: {} Once you send that, I can pick it up from there.",
                jossie_server::events::preview_text(blocker, 320)
            )
        }
        "paused" => format!(
            "I've saved my place on “{title}”.{progress} Just say “continue” whenever you want me to pick it back up."
        ),
        "completed" => format!("That also finishes “{title}”.{progress}"),
        "cancelled" => format!("I've stopped working on “{title}”."),
        _ => match next {
            Some(next) => format!("I'm keeping track of “{title}”.{progress} Next up: {next}."),
            None => format!("I'm keeping track of “{title}”.{progress}"),
        },
    }
}

fn conversational_goals_status(goals: &[jossie_db::GoalWithTasks]) -> String {
    if goals.is_empty() {
        return conversational_goal_status(None);
    }
    if goals.len() == 1 {
        return conversational_goal_status(goals.first());
    }
    let details = goals
        .iter()
        .map(|goal| {
            let progress = if goal.total_tasks == 0 {
                String::new()
            } else {
                format!(
                    " — {}/{} parts done",
                    goal.completed_tasks, goal.total_tasks
                )
            };
            let state = match goal.goal.status.as_str() {
                "blocked" => goal
                    .goal
                    .blocker
                    .as_deref()
                    .map(|blocker| {
                        format!(
                            "; waiting on {}",
                            jossie_server::events::preview_text(blocker, 180)
                        )
                    })
                    .unwrap_or_else(|| "; waiting for more information".to_string()),
                "paused" => "; I've saved my place and can continue when you say so".to_string(),
                _ => goal
                    .tasks
                    .iter()
                    .find(|task| matches!(task.status.as_str(), "in_progress" | "pending"))
                    .map(|task| format!("; next is {}", task.title))
                    .unwrap_or_default(),
            };
            format!("• {}{progress}{state}", goal.goal.title)
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "I'm keeping track of {} things with you:\n{details}",
        goals.len()
    )
}

fn should_continue_tracked_goal(
    goal: &jossie_db::GoalWithTasks,
    content: &str,
    has_attachment: bool,
) -> bool {
    if has_attachment {
        return true;
    }
    let normalized = content
        .trim()
        .trim_matches(|character: char| character.is_ascii_punctuation())
        .to_ascii_lowercase();
    let explicit = matches!(
        normalized.as_str(),
        "continue"
            | "resume"
            | "go on"
            | "keep going"
            | "carry on"
            | "pick it up"
            | "please continue"
            | "yes continue"
            | "yes, continue"
    ) || (!normalized.contains("don't continue")
        && !normalized.contains("do not continue")
        && ["continue", "resume", "keep going", "carry on"]
            .iter()
            .any(|phrase| normalized.starts_with(phrase) || normalized.ends_with(phrase)));
    if explicit {
        return true;
    }
    if goal.goal.status != "blocked" {
        return false;
    }
    let mut blocker_text = goal.goal.blocker.clone().unwrap_or_default();
    for task in goal.tasks.iter().filter(|task| task.status == "blocked") {
        blocker_text.push(' ');
        blocker_text.push_str(task.blocker.as_deref().unwrap_or(&task.title));
    }
    blocker_text
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| {
            word.len() >= 4
                && !matches!(
                    word.as_str(),
                    "that"
                        | "this"
                        | "with"
                        | "from"
                        | "more"
                        | "need"
                        | "send"
                        | "once"
                        | "before"
                        | "continue"
                        | "information"
                )
        })
        .any(|word| normalized.contains(&word))
}

fn conversational_resume_error(error: &anyhow::Error) -> String {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("no paused goal") {
        "I don't have paused work to continue right now.".to_string()
    } else if message.contains("checkpoint") {
        "I can see the paused work, but its saved continuation is no longer available. Tell me what you want to pick up and I'll reconstruct it from our conversation.".to_string()
    } else if message.contains("already") || message.contains("can no longer be resumed") {
        "That work is already being continued or has changed since you asked.".to_string()
    } else {
        "I couldn't pick that work back up just now. I've kept its previous state so we can try again.".to_string()
    }
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
                let goal_before = state
                    .db
                    .get_active_goal_for_conversation(outcome.conversation_id)
                    .await
                    .ok()
                    .flatten();
                match jossie_server::run_agent_loop(&state, outcome.conversation_id).await {
                    Ok(response) => {
                        let goal_after =
                            goal_after_run(&state, outcome.conversation_id, goal_before.as_ref())
                                .await
                                .ok()
                                .flatten();
                        let response = with_conversational_goal_update(
                            response,
                            goal_before.as_ref(),
                            goal_after.as_ref(),
                        );
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
