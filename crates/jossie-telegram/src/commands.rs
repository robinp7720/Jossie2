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
            let text = "Send me a message, photo, document, voice note, or audio file.\n\n/status — see what we're working on\n/new — start a fresh conversation\n/cancel — stop the current run\n/resume — continue paused work\n/help — show this message";
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
        Command::Status => {
            let reply = match state.db.get_telegram_conversation(msg.chat.id.0).await {
                Ok(Some(conversation_id)) => match state
                    .db
                    .list_active_goals_for_conversation(conversation_id)
                    .await
                {
                    Ok(goals) => conversational_goals_status(&goals),
                    Err(error) => {
                        tracing::error!(
                            chat_id = msg.chat.id.0,
                            "Failed to load Telegram goal status: {error}"
                        );
                        "I couldn't check our ongoing work just now.".to_string()
                    }
                },
                Ok(None) => "I don't have any ongoing work in this conversation yet.".to_string(),
                Err(error) => {
                    tracing::error!(
                        chat_id = msg.chat.id.0,
                        "Failed to inspect Telegram conversation: {error}"
                    );
                    "I couldn't check our ongoing work just now.".to_string()
                }
            };
            let _ = send_reply(&bot, msg.chat.id, Some(msg.id), &reply, None).await;
        }
        Command::Resume => {
            if !try_activate_chat(&runtime, msg.chat.id.0).await {
                let _ = send_reply(
                    &bot,
                    msg.chat.id,
                    Some(msg.id),
                    "I'm already working on this conversation.",
                    None,
                )
                .await;
                return;
            }
            let chat_id = msg.chat.id;
            let reply_to = msg.id;
            let numeric_chat_id = msg.chat.id.0;
            tokio::spawn(async move {
                let typing = spawn_typing(bot.clone(), chat_id);
                let result: anyhow::Result<String> = async {
                    let conversation_id = state
                        .db
                        .get_telegram_conversation(numeric_chat_id)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("No linked conversation"))?;
                    let goal = state
                        .db
                        .get_active_goal_for_conversation(conversation_id)
                        .await?
                        .filter(|goal| matches!(goal.goal.status.as_str(), "active" | "paused"))
                        .ok_or_else(|| anyhow::anyhow!("No resumable goal is available"))?;
                    let message = JossieMessage::new(
                        conversation_id,
                        Role::User,
                        format!("Continue the tracked goal: {}", goal.goal.title),
                    )
                    .with_name("goal_resume".to_string());
                    state.db.save_message(&message).await?;
                    let require_checkpoint = goal.goal.status == "paused";
                    continue_tracked_goal(&state, conversation_id, &goal, require_checkpoint).await
                }
                .await;
                let reply = match result {
                    Ok(response) => response,
                    Err(error) => conversational_resume_error(&error),
                };
                let _ = typing.send(());
                let _ = send_reply(&bot, chat_id, Some(reply_to), &reply, None).await;
                release_chat(&runtime, numeric_chat_id).await;
            });
        }
    }
}

async fn continue_tracked_goal(
    state: &Arc<AppState>,
    conversation_id: Uuid,
    goal: &jossie_db::GoalWithTasks,
    require_checkpoint: bool,
) -> anyhow::Result<String> {
    let checkpoint = if require_checkpoint {
        Some(
            state
                .db
                .latest_available_checkpoint_for_goal(&goal.goal.id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("No continuation checkpoint is available"))?,
        )
    } else {
        None
    };
    if goal.goal.status != "active"
        && !state
            .db
            .set_goal_control_state(&goal.goal.id, "resume")
            .await?
    {
        anyhow::bail!("The goal can no longer be resumed");
    }
    let task_id = goal
        .tasks
        .iter()
        .find(|task| !matches!(task.status.as_str(), "completed" | "cancelled"))
        .map(|task| task.id.clone());
    let result = jossie_server::agent::run_agent_loop_when_available(
        state,
        conversation_id,
        jossie_server::agent::AgentRunOptions {
            goal_id: Some(goal.goal.id.clone()),
            task_id,
            work_summary: Some(goal.goal.title.clone()),
            resume_checkpoint_run_id: checkpoint.map(|checkpoint| checkpoint.run_id),
            ..jossie_server::agent::AgentRunOptions::default()
        },
    )
    .await;
    match result {
        Ok(response) => {
            let after = state.db.get_goal_with_tasks(&goal.goal.id).await?;
            Ok(with_conversational_goal_update(
                response,
                Some(goal),
                after.as_ref(),
            ))
        }
        Err(error) => {
            let _ = state
                .db
                .update_goal_metadata(
                    &goal.goal.id,
                    None,
                    None,
                    Some(&goal.goal.status),
                    Some(goal.goal.blocker.as_deref()),
                    None,
                )
                .await;
            Err(error)
        }
    }
}

