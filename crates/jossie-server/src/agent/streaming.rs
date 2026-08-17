pub async fn run_agent_loop_streaming(
    state: &AppState,
    conv_id: Uuid,
    run_id: String,
    source_message_id: Option<Uuid>,
    event_tx: tokio::sync::mpsc::Sender<ServerEvent>,
) {
    if let Err(e) = claim_conversation(state, conv_id).await {
        emit_stream_event(
            state,
            Some(&event_tx),
            ServerEvent::Error {
                conversation_id: conv_id,
                run_id: Some(run_id),
                error: e.to_string(),
            },
        )
        .await;
        return;
    }

    let event_tx_for_error = event_tx.clone();
    let run_id_for_error = run_id.clone();
    let result = AssertUnwindSafe(run_agent_loop_streaming_inner(
        state,
        conv_id,
        run_id,
        source_message_id,
        event_tx,
    ))
        .catch_unwind()
        .await;
    release_conversation(state, conv_id).await;
    let error = match result {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error,
        Err(payload) => {
            let panic_message = panic_payload_to_string(payload);
            tracing::error!(
                "Streaming agent loop panicked for conversation {conv_id}: {panic_message}"
            );
            anyhow::anyhow!("Agent loop panicked: {panic_message}")
        }
    };
    emit_stream_event(
        state,
        Some(&event_tx_for_error),
        ServerEvent::Error {
            conversation_id: conv_id,
            run_id: Some(run_id_for_error),
            error: error.to_string(),
        },
    )
    .await;
}

async fn run_agent_loop_streaming_inner(
    state: &AppState,
    conv_id: Uuid,
    run_id: String,
    source_message_id: Option<Uuid>,
    event_tx: tokio::sync::mpsc::Sender<ServerEvent>,
) -> anyhow::Result<()> {
    let options = AgentRunOptions::default();
    let (
        mut toolset,
        mut messages,
        last_user_msg,
        mut goal_tracker,
        mut reflection_retries_remaining,
        prompt_cache_key,
    ) = match prepare_run_context(state, conv_id, &options).await {
        Ok(ctx) => ctx,
        Err(e) => return Err(e),
    };

    emit_stream_event(
        state,
        Some(&event_tx),
        ServerEvent::RunStarted {
            conversation_id: conv_id,
            run_id: run_id.clone(),
            scheduled: false,
        },
    )
    .await;
    if let Some(source_message_id) = source_message_id
        && let Err(error) = state
            .db
            .annotate_work_run(
                &run_id,
                None,
                None,
                Some("chat_message"),
                Some(&source_message_id.to_string()),
                None,
                None,
            )
            .await
    {
        tracing::warn!("Failed to associate chat run {run_id} with message: {error}");
    }

    let mut previous_response_id: Option<String> = None;
    let mut chained_messages = Vec::new();
    let run_started = std::time::Instant::now();
    let mut cumulative_tokens = 0u64;
    let mut premature_goal_finals = 0usize;

    for iteration in 0..state.agent.max_agent_iterations {
        if ensure_run_not_cancelled(state, conv_id, &run_id, Some(&event_tx))
            .await
            .is_err()
        {
            return Ok(());
        }
        if iteration > 0
            && run_started.elapsed() >= Duration::from_secs(state.agent.interactive_run_budget_seconds)
        {
            if let Ok(partial) = pause_run_with_checkpoint(
                state,
                conv_id,
                &run_id,
                &goal_tracker,
                "Interactive time budget reached",
                Some(&event_tx),
            )
            .await
            {
                emit_stream_event(
                    state,
                    Some(&event_tx),
                    ServerEvent::AssistantDelta {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                        content: partial,
                    },
                )
                .await;
            }
            return Ok(());
        }
        if iteration > 0 {
            inject_goal_tracking_message(&mut messages, &goal_tracker);
            chained_messages.push(
                Message::transient(Role::System, goal_tracker.build_tracking_message())
                    .with_name("goal_tracker".to_string()),
            );
            bound_context_window(
                &mut messages,
                state.agent.max_context_chars,
                state.agent.context_compact_target_chars,
                state.agent.context_keep_recent_dialogue_messages,
            );
        }

        emit_stream_event(
            state,
            Some(&event_tx),
            ServerEvent::AssistantThinking {
                conversation_id: conv_id,
                run_id: run_id.clone(),
                iteration,
            },
        )
        .await;

        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel(200);
        let llm = state.llm.clone();
        let (messages_clone, request_options) = if state.agent.openai_optimizations
            && let Some(previous_response_id) = previous_response_id.as_ref()
        {
            (
                chained_messages.clone(),
                jossie_llm::LlmRequestOptions {
                    previous_response_id: Some(previous_response_id.clone()),
                    ..jossie_llm::LlmRequestOptions::default()
                },
            )
        } else {
            (
                messages.clone(),
                initial_llm_request_options(state, &prompt_cache_key),
            )
        };
        let tools = toolset.definitions(state);
        let tools_clone = tools.clone();
        let continuation_attempt = request_options.previous_response_id.is_some();

        let stream_task = tokio::spawn(async move {
            if let Err(e) = llm
                .complete_stream_with_options(
                    &messages_clone,
                    &tools_clone,
                    &request_options,
                    stream_tx,
                )
                .await
            {
                tracing::error!("LLM stream error: {e}");
            }
        });

        let mut full_content = String::new();
        let mut tool_calls = Vec::new();
        let mut response_items = Vec::new();
        let mut stream_error = None;
        let mut done_received = false;
        let mut completed_response_id = None;
        let stream_deadline =
            tokio::time::Instant::now() + Duration::from_secs(state.agent.llm_request_timeout_seconds);

        while !done_received {
            tokio::select! {
                maybe_event = stream_rx.recv() => {
                    match maybe_event {
                        Some(jossie_llm::StreamEvent::Delta(delta)) => {
                            full_content.push_str(&delta);
                            emit_stream_event(
                                state,
                                Some(&event_tx),
                                ServerEvent::AssistantDelta {
                                    conversation_id: conv_id,
                                    run_id: run_id.clone(),
                                    content: delta,
                                },
                            ).await;
                        }
                        Some(jossie_llm::StreamEvent::Completed {
                            tool_calls: calls,
                            response_items: items,
                            response_id,
                            usage,
                        }) => {
                            if let Some(usage) = usage {
                                cumulative_tokens = cumulative_tokens.saturating_add(usage.total_tokens);
                                tracing::info!(
                                    conversation_id = %conv_id,
                                    run_id = %run_id,
                                    iteration,
                                    request_input_tokens = usage.input_tokens,
                                    request_total_tokens = usage.total_tokens,
                                    cumulative_tokens,
                                    "Streaming agent run token usage"
                                );
                            }
                            tool_calls = calls;
                            response_items = items;
                            completed_response_id = response_id;
                            done_received = true;
                        }
                        Some(jossie_llm::StreamEvent::Error(e)) => {
                            stream_error = Some(e);
                            done_received = true;
                        }
                        None => {
                            done_received = true;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if state.is_cancel_requested(conv_id).await {
                        stream_task.abort();
                        let _ = ensure_run_not_cancelled(state, conv_id, &run_id, Some(&event_tx)).await;
                        return Ok(());
                    }
                }
                _ = tokio::time::sleep_until(stream_deadline) => {
                    stream_task.abort();
                    stream_error = Some(format!(
                        "LLM stream timed out after {} seconds",
                        state.agent.llm_request_timeout_seconds
                    ));
                    done_received = true;
                }
            }
        }

        let _ = stream_task.await;

        if let Some(error) = stream_error {
            if continuation_attempt && full_content.is_empty() {
                tracing::warn!(
                    "Streaming Responses continuation failed; retrying from local context: {error}"
                );
                let request_options = initial_llm_request_options(state, &prompt_cache_key);
                match state
                    .llm
                    .complete_with_options(&messages, &tools, &request_options)
                    .await
                {
                    Ok(output) => {
                        full_content = output.content;
                        tool_calls = output.tool_calls;
                        response_items = output.response_items;
                        completed_response_id = output.response_id;
                        if !full_content.is_empty() {
                            emit_stream_event(
                                state,
                                Some(&event_tx),
                                ServerEvent::AssistantDelta {
                                    conversation_id: conv_id,
                                    run_id: run_id.clone(),
                                    content: full_content.clone(),
                                },
                            )
                            .await;
                        }
                    }
                    Err(fallback_error) => {
                        emit_stream_event(
                            state,
                            Some(&event_tx),
                            ServerEvent::Error {
                                conversation_id: conv_id,
                                run_id: Some(run_id.clone()),
                                error: fallback_error.to_string(),
                            },
                        )
                        .await;
                        return Ok(());
                    }
                }
            } else {
                emit_stream_event(
                    state,
                    Some(&event_tx),
                    ServerEvent::Error {
                        conversation_id: conv_id,
                        run_id: Some(run_id.clone()),
                        error,
                    },
                )
                .await;
                return Ok(());
            }
        }
        previous_response_id = completed_response_id;
        chained_messages.clear();
        match process_agent_iteration_output(
            state,
            conv_id,
            &run_id,
            full_content,
            tool_calls,
            response_items,
            Some(&event_tx),
            &last_user_msg,
            &last_user_msg,
            &mut messages,
            &mut chained_messages,
            &mut previous_response_id,
            &mut toolset,
            &mut goal_tracker,
            &mut reflection_retries_remaining,
            &mut premature_goal_finals,
        )
        .await?
        {
            AgentIterationOutcome::Continue => continue,
            AgentIterationOutcome::Final(_) => {
                emit_stream_event(
                    state,
                    Some(&event_tx),
                    ServerEvent::RunCompleted {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                    },
                )
                .await;
                return Ok(());
            }
            AgentIterationOutcome::WaitingForApproval(_)
            | AgentIterationOutcome::Paused(_)
            | AgentIterationOutcome::Cancelled => return Ok(()),
        }
    }

    let partial = pause_run_with_checkpoint(
        state,
        conv_id,
        &run_id,
        &goal_tracker,
        &format!(
            "Agent iteration budget of {} reached",
            state.agent.max_agent_iterations
        ),
        Some(&event_tx),
    )
    .await?;
    emit_stream_event(
        state,
        Some(&event_tx),
        ServerEvent::AssistantDelta {
            conversation_id: conv_id,
            run_id,
            content: partial,
        },
    )
    .await;
    Ok(())
}
