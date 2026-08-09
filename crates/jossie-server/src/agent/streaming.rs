/// Run the agent loop with streaming, sending events to the caller via an mpsc channel.
/// This is the streaming counterpart of `run_agent_loop`.
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
    if let Err(payload) = result {
        let panic_message = panic_payload_to_string(payload);
        tracing::error!(
            "Streaming agent loop panicked for conversation {conv_id}: {panic_message}"
        );
        emit_stream_event(
            state,
            Some(&event_tx_for_error),
            ServerEvent::Error {
                conversation_id: conv_id,
                run_id: None,
                error: format!("Agent loop panicked: {panic_message}"),
            },
        )
        .await;
    }
}

async fn run_agent_loop_streaming_inner(
    state: &AppState,
    conv_id: Uuid,
    run_id: String,
    source_message_id: Option<Uuid>,
    event_tx: tokio::sync::mpsc::Sender<ServerEvent>,
) {
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
        Err(e) => {
            emit_stream_event(
                state,
                Some(&event_tx),
                ServerEvent::Error {
                    conversation_id: conv_id,
                    run_id: Some(run_id.clone()),
                    error: e.to_string(),
                },
            )
            .await;
            return;
        }
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

    for iteration in 0..state.max_agent_iterations {
        if ensure_run_not_cancelled(state, conv_id, &run_id, Some(&event_tx))
            .await
            .is_err()
        {
            return;
        }
        if iteration > 0
            && run_started.elapsed() >= Duration::from_secs(state.interactive_run_budget_seconds)
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
            return;
        }
        if iteration > 0 {
            inject_goal_tracking_message(&mut messages, &goal_tracker);
            chained_messages.push(
                Message::transient(Role::System, goal_tracker.build_tracking_message())
                    .with_name("goal_tracker".to_string()),
            );
            bound_context_window(
                &mut messages,
                state.max_context_chars,
                state.context_compact_target_chars,
                state.context_keep_recent_dialogue_messages,
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
        let (messages_clone, request_options) = if state.openai_optimizations
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
            tokio::time::Instant::now() + Duration::from_secs(state.llm_request_timeout_seconds);

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
                        return;
                    }
                }
                _ = tokio::time::sleep_until(stream_deadline) => {
                    stream_task.abort();
                    stream_error = Some(format!(
                        "LLM stream timed out after {} seconds",
                        state.llm_request_timeout_seconds
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
                        return;
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
                return;
            }
        }
        previous_response_id = completed_response_id;
        chained_messages.clear();

        if !tool_calls.is_empty() {
            if let Some(loop_warning) = goal_tracker.note_tool_batch(&tool_calls) {
                tracing::warn!(
                    "Streaming loop guard triggered for conversation {conv_id}: {loop_warning}"
                );
                emit_stream_event(
                    state,
                    Some(&event_tx),
                    ServerEvent::AssistantReset {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                        reason: "loop_guard".to_string(),
                    },
                )
                .await;

                if goal_tracker.should_stop_for_repetition() {
                    if goal_tracker.active_goal_continuation_message().is_some() {
                        if let Ok(partial) = pause_run_with_checkpoint(
                            state,
                            conv_id,
                            &run_id,
                            &goal_tracker,
                            "The agent repeated the same action without advancing its tracked goal",
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
                        return;
                    }
                    let fallback = goal_tracker.build_stuck_message();
                    emit_stream_event(
                        state,
                        Some(&event_tx),
                        ServerEvent::AssistantDelta {
                            conversation_id: conv_id,
                            run_id: run_id.clone(),
                            content: fallback.clone(),
                        },
                    )
                    .await;
                    let assistant_msg = Message::new(conv_id, Role::Assistant, fallback);
                    let _ = persist_message(state, &assistant_msg).await;
                    emit_stream_event(
                        state,
                        Some(&event_tx),
                        ServerEvent::RunCompleted {
                            conversation_id: conv_id,
                            run_id: run_id.clone(),
                        },
                    )
                    .await;
                    return;
                }

                messages.push(Message::transient(Role::Assistant, full_content));
                messages.push(Message::transient(
                    Role::System,
                    format!("[LOOP GUARD: {loop_warning}]"),
                ));
                previous_response_id = None;
                continue;
            }

            let assistant_msg = match serde_json::to_value(&tool_calls) {
                Ok(tc_json) => Message::new(conv_id, Role::Assistant, full_content.clone())
                    .with_tool_calls(tc_json)
                    .with_response_items(response_items),
                Err(_) => Message::new(conv_id, Role::Assistant, full_content.clone()),
            };
            let _ = persist_message(state, &assistant_msg).await;
            messages.push(assistant_msg);

            let capability_message_start = messages.len();
            match process_work_plan_updates(
                state,
                conv_id,
                &run_id,
                Some(&event_tx),
                &tool_calls,
                &mut messages,
                &mut goal_tracker,
            )
            .await
            {
                Ok(true) => {
                    chained_messages.extend_from_slice(&messages[capability_message_start..]);
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    emit_stream_event(
                        state,
                        Some(&event_tx),
                        ServerEvent::Error {
                            conversation_id: conv_id,
                            run_id: Some(run_id.clone()),
                            error: error.to_string(),
                        },
                    )
                    .await;
                    return;
                }
            }
            match process_capability_activation(
                state,
                conv_id,
                &run_id,
                Some(&event_tx),
                &mut toolset,
                &tool_calls,
                &mut messages,
                &mut goal_tracker,
            )
            .await
            {
                Ok(true) => {
                    chained_messages.extend_from_slice(&messages[capability_message_start..]);
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    emit_stream_event(
                        state,
                        Some(&event_tx),
                        ServerEvent::Error {
                            conversation_id: conv_id,
                            run_id: Some(run_id.clone()),
                            error: error.to_string(),
                        },
                    )
                    .await;
                    return;
                }
            }

            let prepared_calls = prepare_tool_calls_for_execution(
                &tool_calls,
                conv_id,
                &last_user_msg,
                goal_tracker.durable_goal.as_ref(),
            );
            let (prepared_calls, pending_actions) = match partition_authorized_calls(
                state,
                conv_id,
                &run_id,
                Some(&event_tx),
                prepared_calls,
                &last_user_msg,
                &messages,
            )
            .await
            {
                Ok(partition) => partition,
                Err(error) => {
                    emit_stream_event(
                        state,
                        Some(&event_tx),
                        ServerEvent::Error {
                            conversation_id: conv_id,
                            run_id: Some(run_id.clone()),
                            error: error.to_string(),
                        },
                    )
                    .await;
                    return;
                }
            };
            let (prepared_calls, repeated_results) =
                goal_tracker.split_repeated_reads(prepared_calls);

            for call in &prepared_calls {
                emit_stream_event(
                    state,
                    Some(&event_tx),
                    ServerEvent::ToolCalled {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                        call_id: call.id.clone(),
                        tool: call.name.clone(),
                        arguments_preview: preview_text(&call.arguments, 160),
                    },
                )
                .await;
                let started_event = ServerEvent::ToolStarted {
                    conversation_id: conv_id,
                    run_id: run_id.clone(),
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                };
                emit_stream_event(state, Some(&event_tx), started_event).await;
            }
            let mut results = execute_tool_batch(state, conv_id, prepared_calls).await;
            results.extend(repeated_results);
            if ensure_run_not_cancelled(state, conv_id, &run_id, Some(&event_tx))
                .await
                .is_err()
            {
                return;
            }

            for (_, call, result) in results {
                emit_stream_event(
                    state,
                    Some(&event_tx),
                    ServerEvent::ToolFinished {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                        call_id: call.id.clone(),
                        tool: call.name.clone(),
                        result_preview: preview_text(&result.content, 220),
                        is_error: result.is_error,
                    },
                )
                .await;
                goal_tracker.record_tool_result(&call, &result);
                let tool_msg = Message::new(conv_id, Role::Tool, result.content)
                    .with_tool_call_id(call.id.clone())
                    .with_name(call.name.clone());
                let _ = persist_message(state, &tool_msg).await;
                messages.push(tool_msg.clone());
                chained_messages.push(tool_msg);
            }
            if let Some(action) = pending_actions.first() {
                emit_stream_event(
                    state,
                    Some(&event_tx),
                    ServerEvent::RunWaitingForApproval {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                        batch_id: action.batch_id.clone(),
                    },
                )
                .await;
                return;
            }
            goal_tracker.record_tool_calls(&tool_calls);
            continue;
        }

        if reflection_retries_remaining > 0 {
            if let Some(feedback) =
                self_reflect(state, &messages, &last_user_msg, &full_content).await
            {
                reflection_retries_remaining -= 1;
                emit_stream_event(
                    state,
                    Some(&event_tx),
                    ServerEvent::ReflectionRetry {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                        feedback: feedback.clone(),
                    },
                )
                .await;
                emit_stream_event(
                    state,
                    Some(&event_tx),
                    ServerEvent::AssistantReset {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                        reason: "reflection_retry".to_string(),
                    },
                )
                .await;
                messages.push(
                    Message::transient(Role::Assistant, full_content)
                        .with_response_items(response_items),
                );
                let feedback_message = Message::transient(
                    Role::System,
                    format!(
                        "[SELF-REFLECTION FEEDBACK: Your response needs improvement. {}. Please revise your response.]",
                        feedback
                    ),
                );
                messages.push(feedback_message.clone());
                chained_messages.push(feedback_message);
                continue;
            }
        }

        if let Some(continuation) = goal_tracker.active_goal_continuation_message() {
            premature_goal_finals += 1;
            tracing::warn!(
                conversation_id = %conv_id,
                run_id = %run_id,
                attempt = premature_goal_finals,
                "Withholding a streamed final reply because its tracked goal is still active"
            );
            emit_stream_event(
                state,
                Some(&event_tx),
                ServerEvent::AssistantReset {
                    conversation_id: conv_id,
                    run_id: run_id.clone(),
                    reason: "active_goal_continuation".to_string(),
                },
            )
            .await;
            if premature_goal_finals >= PREMATURE_GOAL_FINAL_LIMIT {
                if let Ok(partial) = pause_run_with_checkpoint(
                    state,
                    conv_id,
                    &run_id,
                    &goal_tracker,
                    "The agent repeatedly stopped while its tracked goal was still active",
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
                return;
            }
            messages.push(
                Message::transient(Role::Assistant, full_content)
                    .with_response_items(response_items),
            );
            let continuation = Message::transient(Role::System, continuation)
                .with_name("active_goal_continuation".to_string());
            messages.push(continuation.clone());
            chained_messages.push(continuation);
            continue;
        }

        let _ = persist_final_assistant_response(
            state,
            conv_id,
            last_user_msg.clone(),
            full_content,
        )
        .await;

        emit_stream_event(
            state,
            Some(&event_tx),
            ServerEvent::RunCompleted {
                conversation_id: conv_id,
                run_id: run_id.clone(),
            },
        )
        .await;
        return;
    }

    match pause_run_with_checkpoint(
        state,
        conv_id,
        &run_id,
        &goal_tracker,
        &format!(
            "Agent iteration budget of {} reached",
            state.max_agent_iterations
        ),
        Some(&event_tx),
    )
    .await
    {
        Ok(partial) => {
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
        }
        Err(error) => {
            emit_stream_event(
                state,
                Some(&event_tx),
                ServerEvent::Error {
                    conversation_id: conv_id,
                    run_id: Some(run_id),
                    error: error.to_string(),
                },
            )
            .await;
        }
    }
}
