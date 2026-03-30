use crate::events::{ServerEvent, persist_message, preview_text};
use crate::state::AppState;
use jossie_core::types::{Message, Role};
use jossie_db::{IntegrationEvent, MemoryKeyInfo};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

// --- Goal Tracking (#4) ---

struct GoalTracker {
    primary_goal: String,
    completed_steps: Vec<String>,
}

impl GoalTracker {
    fn new(user_message: &str) -> Self {
        // Extract a concise goal from the user message (first 200 chars)
        let goal = if user_message.len() > 200 {
            format!("{}...", &user_message[..200])
        } else {
            user_message.to_string()
        };
        Self {
            primary_goal: goal,
            completed_steps: Vec::new(),
        }
    }

    fn record_tool_calls(&mut self, calls: &[jossie_core::ToolCall]) {
        for call in calls {
            self.completed_steps.push(format!(
                "Called `{}` with args: {}",
                call.name,
                if call.arguments.len() > 100 {
                    format!("{:.100}...", call.arguments)
                } else {
                    call.arguments.clone()
                }
            ));
        }
    }

    fn build_tracking_message(&self) -> String {
        let mut msg = format!(
            "## Goal Tracking\n**Primary Goal:** {}\n",
            self.primary_goal
        );
        if !self.completed_steps.is_empty() {
            msg.push_str("**Completed Steps:**\n");
            for (i, step) in self.completed_steps.iter().enumerate() {
                msg.push_str(&format!("{}. {}\n", i + 1, step));
            }
        }
        msg.push_str(
            "**Next:** Decide if the goal is fully addressed or if more tool calls are needed.",
        );
        msg
    }
}

#[derive(Debug, Clone)]
pub struct AgentRunOptions {
    pub allow_schedule_management: bool,
    pub allow_oob_messages: bool,
    pub scheduled_execution: bool,
}

impl Default for AgentRunOptions {
    fn default() -> Self {
        Self {
            allow_schedule_management: true,
            allow_oob_messages: true,
            scheduled_execution: false,
        }
    }
}

async fn build_system_prompt(state: &AppState, user_message: Option<&str>) -> String {
    let mut prompt = state.system_prompt.clone();

    // Add current time context
    let now = chrono::Local::now();
    prompt.push_str(&format!(
        "\n\nCurrent Date and Time: {}",
        now.format("%A, %B %d, %Y %H:%M:%S")
    ));

    // Dynamically append agent and user profiles from memory

    // Core Identity (Soul) - High Priority
    if let Ok(Some(entry)) = state.db.get_memory("agent_profile.soul").await {
        prompt.push_str("\n\n## Agent Core Identity (Soul)\n");
        prompt.push_str(&entry.content);
    }

    if let Ok(Some(entry)) = state.db.get_memory("agent_profile").await {
        prompt.push_str("\n\n## Agent Description (Jossie)\n");
        prompt.push_str(&entry.content);
    }

    // Current Mood/State
    if let Ok(Some(entry)) = state.db.get_memory("agent_profile.mood").await {
        prompt.push_str("\n\n## Current Mood\n");
        prompt.push_str(&entry.content);
    }

    if let Ok(Some(entry)) = state.db.get_memory("user_profile").await {
        prompt.push_str("\n\n## User Description\n");
        prompt.push_str(&entry.content);
    }

    match state.db.memory_list_keys().await {
        Ok(memory_keys) => {
            prompt.push_str("\n\n");
            prompt.push_str(&format_memory_index(&memory_keys));
        }
        Err(err) => {
            tracing::warn!("Failed to build memory index for prompt: {err}");
        }
    }

    if let Some(message) = user_message {
        let graph_context = build_graph_context(state, message).await;
        if !graph_context.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&graph_context);
        }
    }

    tracing::debug!("System Prompt Built. Length: {} chars", prompt.len());
    prompt
}

fn format_memory_index(keys: &[MemoryKeyInfo]) -> String {
    if keys.is_empty() {
        return "## Memory Index (All Available Memories)\nNo memories are currently saved. Use memory tools to store durable context when useful.".to_string();
    }

    let mut section = String::from(
        "## Memory Index (All Available Memories)\nUse this dynamic list to fill context gaps before asking the user to repeat information.\n",
    );

    for key_info in keys {
        section.push_str("- `");
        section.push_str(&key_info.key);
        section.push('`');
        if !key_info.updated_at.is_empty() {
            section.push_str(" (updated ");
            section.push_str(&key_info.updated_at);
            section.push(')');
        }
        section.push('\n');
    }

    section
}

pub async fn prepend_system_prompt(
    state: &AppState,
    messages: &mut Vec<Message>,
    user_message: Option<&str>,
) {
    let content = build_system_prompt(state, user_message).await;
    if content.is_empty() {
        return;
    }
    messages.insert(0, Message::transient(Role::System, content));
}

pub async fn run_agent_loop(state: &AppState, conv_id: Uuid) -> anyhow::Result<String> {
    run_agent_loop_with_options(state, conv_id, AgentRunOptions::default()).await
}

async fn claim_conversation(state: &AppState, conv_id: Uuid) -> anyhow::Result<()> {
    let mut active = state.active_conversations.write().await;
    if !active.insert(conv_id) {
        anyhow::bail!("Conversation {} is already being processed", conv_id);
    }
    drop(active);
    state.clear_cancel(conv_id).await;
    Ok(())
}

async fn release_conversation(state: &AppState, conv_id: Uuid) {
    let mut active = state.active_conversations.write().await;
    active.remove(&conv_id);
    drop(active);
    state.clear_cancel(conv_id).await;
}

fn build_tools_for_options(
    state: &AppState,
    options: &AgentRunOptions,
) -> Vec<jossie_core::ToolDefinition> {
    let mut tools = state.registry.all_tool_definitions();
    if !options.allow_schedule_management {
        tools.retain(|tool| tool.name != "schedule_task" && tool.name != "schedule_recurring_task");
    }
    if !options.allow_oob_messages {
        tools.retain(|tool| tool.name != "send_user_message");
    }
    tools
}

async fn prepare_run_context(
    state: &AppState,
    conv_id: Uuid,
    options: &AgentRunOptions,
) -> anyhow::Result<(Vec<jossie_core::ToolDefinition>, Vec<Message>, String, GoalTracker, usize)> {
    let tools = build_tools_for_options(state, options);

    let mut messages = state
        .db
        .get_messages(conv_id, Some(state.max_context_messages))
        .await?;

    let last_user_msg = messages
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default();

    sanitize_context_window(&mut messages);
    maybe_summarize_context(state, conv_id, &mut messages).await;
    sanitize_context_window(&mut messages);
    prepend_system_prompt(state, &mut messages, Some(&last_user_msg)).await;
    if options.scheduled_execution {
        messages.insert(1, Message::transient(
            Role::System,
            "Scheduled execution mode: this turn was triggered by an existing schedule. Execute the task now and do not create new schedules unless the user explicitly asks in this same turn.".to_string(),
        ).with_name("scheduled_execution_mode".to_string()));
    }

    Ok((
        tools,
        messages,
        last_user_msg.clone(),
        GoalTracker::new(&last_user_msg),
        if state.enable_self_reflection { 1 } else { 0 },
    ))
}

fn inject_goal_tracking_message(messages: &mut Vec<Message>, goal_tracker: &GoalTracker) {
    messages.retain(|m| {
        !(m.role == Role::System
            && m.name.as_deref() == Some("goal_tracker")
            && m.tool_call_id.is_none())
    });
    let tracking_msg = Message::transient(Role::System, goal_tracker.build_tracking_message())
        .with_name("goal_tracker".to_string());
    messages.insert(1, tracking_msg);
}

async fn emit_stream_event(
    event_tx: Option<&tokio::sync::mpsc::Sender<ServerEvent>>,
    event: ServerEvent,
) {
    if let Some(tx) = event_tx {
        let _ = tx.send(event).await;
    }
}

async fn ensure_run_not_cancelled(
    state: &AppState,
    conv_id: Uuid,
    run_id: &str,
    event_tx: Option<&tokio::sync::mpsc::Sender<ServerEvent>>,
) -> anyhow::Result<()> {
    if state.is_cancel_requested(conv_id).await {
        emit_stream_event(
            event_tx,
            ServerEvent::RunCancelled {
                conversation_id: conv_id,
                run_id: run_id.to_string(),
            },
        )
        .await;
        anyhow::bail!("Conversation {} run cancelled", conv_id);
    }
    Ok(())
}

fn prepare_tool_calls_for_execution(
    tool_calls: &[jossie_core::ToolCall],
    conv_id: Uuid,
) -> Vec<jossie_core::ToolCall> {
    tool_calls
        .iter()
        .map(|call| {
            let mut call_with_context = call.clone();
            if call.name.starts_with("schedule_")
                || call.name == "send_user_message"
                || call.name == "list_scheduled_tasks"
            {
                if let Ok(mut args) = serde_json::from_str::<serde_json::Value>(&call.arguments) {
                    if let Some(obj) = args.as_object_mut() {
                        obj.insert(
                            "__conversation_id".to_string(),
                            serde_json::Value::String(conv_id.to_string()),
                        );
                        if let Ok(json_str) = serde_json::to_string(&args) {
                            call_with_context.arguments = json_str;
                        }
                    }
                }
            }
            call_with_context
        })
        .collect()
}

pub async fn run_agent_loop_with_options(
    state: &AppState,
    conv_id: Uuid,
    options: AgentRunOptions,
) -> anyhow::Result<String> {
    // Try to claim this conversation
    claim_conversation(state, conv_id).await?;

    // Execute the agent loop and ensure we release the lock even on panic/error
    let result = run_agent_loop_inner(state, conv_id, &options).await;

    // Release the conversation lock
    release_conversation(state, conv_id).await;

    result
}

async fn run_agent_loop_inner(
    state: &AppState,
    conv_id: Uuid,
    options: &AgentRunOptions,
) -> anyhow::Result<String> {
    let run_id = Uuid::new_v4().to_string();
    let (tools, mut messages, last_user_msg, mut goal_tracker, mut reflection_retries_remaining) =
        prepare_run_context(state, conv_id, options).await?;

    for _iteration in 0..state.max_agent_iterations {
        ensure_run_not_cancelled(state, conv_id, &run_id, None).await?;
        if _iteration > 0 {
            inject_goal_tracking_message(&mut messages, &goal_tracker);
        }
        let total_chars: usize = messages.iter().map(|m| m.content.len()).sum();
        let est_tokens = total_chars / 4;
        tracing::info!(
            "Agent Loop Iteration {}. Messages: {}. Total Chars: {}. Est Tokens: {}",
            _iteration,
            messages.len(),
            total_chars,
            est_tokens
        );

        if est_tokens > 200_000 {
            tracing::warn!("⚠️ CONTEXT SIZE WARNING: Context is very large!");
            // Optional: Print top 3 largest messages
            let mut sizes: Vec<(usize, String)> = messages
                .iter()
                .enumerate()
                .map(|(i, m)| (m.content.len(), format!("Msg[{}] Role: {:?}", i, m.role)))
                .collect();
            sizes.sort_by(|a, b| b.0.cmp(&a.0));
            for (size, info) in sizes.iter().take(3) {
                tracing::warn!("   Large Message: {} chars - {}", size, info);
            }
        }

        let (content, tool_calls) = state.llm.complete(&messages, &tools).await?;

        if tool_calls.is_empty() {
            if reflection_retries_remaining > 0 {
                if let Some(feedback) = self_reflect(state, &last_user_msg, &content).await {
                    reflection_retries_remaining -= 1;
                    tracing::info!("Self-reflection retry. Feedback: {feedback}");
                    // Add the assistant's response and feedback, then continue the loop
                    messages.push(Message::transient(Role::Assistant, content.clone()));
                    messages.push(Message::transient(
                        Role::System,
                        format!(
                            "[SELF-REFLECTION FEEDBACK: Your response needs improvement. {}. Please revise your response.]",
                            feedback
                        ),
                    ));
                    continue;
                }
            }

            let msg = Message::new(conv_id, Role::Assistant, content.clone());
            persist_message(state, &msg).await?;

            let db = state.db.clone();
            let kg_llm = state.kg_llm.clone();
            let assistant_reply = content.clone();

            tokio::spawn(async move {
                spawn_knowledge_extraction(db, kg_llm, last_user_msg, assistant_reply).await;
            });

            return Ok(content);
        }

        goal_tracker.record_tool_calls(&tool_calls);

        let tc_json = serde_json::to_value(&tool_calls)?;
        let assistant_msg =
            Message::new(conv_id, Role::Assistant, content.clone()).with_tool_calls(tc_json);
        persist_message(state, &assistant_msg).await?;
        messages.push(assistant_msg);

        let prepared_calls = prepare_tool_calls_for_execution(&tool_calls, conv_id);

        let mut join_set = tokio::task::JoinSet::new();
        for (idx, call) in prepared_calls.into_iter().enumerate() {
            let registry = state.registry.clone();
            tracing::info!(
                "Executing tool: {} with args: {}",
                call.name,
                call.arguments
            );
            join_set.spawn(async move {
                let result = registry.execute(&call).await;
                (idx, call, result)
            });
        }

        let mut results: Vec<(usize, jossie_core::ToolCall, jossie_core::ToolResult)> =
            Vec::with_capacity(tool_calls.len());
        while let Some(res) = join_set.join_next().await {
            ensure_run_not_cancelled(state, conv_id, &run_id, None).await?;
            match res {
                Ok(tuple) => results.push(tuple),
                Err(e) => tracing::error!("Tool task panicked: {e}"),
            }
        }
        results.sort_by_key(|(idx, _, _)| *idx);

        for (_, call, result) in results {
            tracing::info!(
                "Tool {} finished. Result preview: {:.200}...",
                call.name,
                result.content
            );
            let tool_msg = Message::new(conv_id, Role::Tool, result.content)
                .with_tool_call_id(call.id.clone())
                .with_name(call.name.clone());
            persist_message(state, &tool_msg).await?;
            messages.push(tool_msg);
        }
    }

    anyhow::bail!(
        "Agent loop exceeded maximum of {} iterations",
        state.max_agent_iterations
    )
}

/// Run the agent loop with streaming, sending events to the caller via an mpsc channel.
/// This is the streaming counterpart of `run_agent_loop`.
pub async fn run_agent_loop_streaming(
    state: &AppState,
    conv_id: Uuid,
    event_tx: tokio::sync::mpsc::Sender<ServerEvent>,
) {
    if let Err(e) = claim_conversation(state, conv_id).await {
        let _ = event_tx
            .send(ServerEvent::Error {
                conversation_id: conv_id,
                run_id: None,
                error: e.to_string(),
            })
            .await;
        return;
    }

    run_agent_loop_streaming_inner(state, conv_id, event_tx).await;
    release_conversation(state, conv_id).await;
}

async fn run_agent_loop_streaming_inner(
    state: &AppState,
    conv_id: Uuid,
    event_tx: tokio::sync::mpsc::Sender<ServerEvent>,
) {
    let run_id = Uuid::new_v4().to_string();
    let options = AgentRunOptions::default();
    let (tools, mut messages, last_user_msg, mut goal_tracker, mut reflection_retries_remaining) =
        match prepare_run_context(state, conv_id, &options).await {
            Ok(ctx) => ctx,
            Err(e) => {
                let _ = event_tx
                    .send(ServerEvent::Error {
                        conversation_id: conv_id,
                        run_id: Some(run_id.clone()),
                        error: e.to_string(),
                    })
                    .await;
                return;
            }
        };

    emit_stream_event(
        Some(&event_tx),
        ServerEvent::RunStarted {
            conversation_id: conv_id,
            run_id: run_id.clone(),
            scheduled: false,
        },
    )
    .await;

    for iteration in 0..state.max_agent_iterations {
        if ensure_run_not_cancelled(state, conv_id, &run_id, Some(&event_tx))
            .await
            .is_err()
        {
            return;
        }
        if iteration > 0 {
            inject_goal_tracking_message(&mut messages, &goal_tracker);
        }

        emit_stream_event(
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
        let messages_clone = messages.clone();
        let tools_clone = tools.clone();

        let stream_task = tokio::spawn(async move {
            if let Err(e) = llm
                .complete_stream(&messages_clone, &tools_clone, stream_tx)
                .await
            {
                tracing::error!("LLM stream error: {e}");
            }
        });

        let mut full_content = String::new();
        let mut tool_calls = Vec::new();
        let mut stream_failed = false;
        let mut done_received = false;

        while !done_received {
            tokio::select! {
                maybe_event = stream_rx.recv() => {
                    match maybe_event {
                        Some(jossie_llm::StreamEvent::Delta(delta)) => {
                            full_content.push_str(&delta);
                            emit_stream_event(
                                Some(&event_tx),
                                ServerEvent::AssistantDelta {
                                    conversation_id: conv_id,
                                    run_id: run_id.clone(),
                                    content: delta,
                                },
                            ).await;
                        }
                        Some(jossie_llm::StreamEvent::ToolCalls(calls)) => {
                            tool_calls = calls;
                            done_received = true;
                        }
                        Some(jossie_llm::StreamEvent::Done) => {
                            done_received = true;
                        }
                        Some(jossie_llm::StreamEvent::Error(e)) => {
                            stream_failed = true;
                            done_received = true;
                            emit_stream_event(
                                Some(&event_tx),
                                ServerEvent::Error {
                                    conversation_id: conv_id,
                                    run_id: Some(run_id.clone()),
                                    error: e,
                                },
                            ).await;
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
            }
        }

        let _ = stream_task.await;

        if stream_failed {
            return;
        }

        if !tool_calls.is_empty() {
            if iteration + 1 >= state.max_agent_iterations {
                emit_stream_event(
                    Some(&event_tx),
                    ServerEvent::Error {
                        conversation_id: conv_id,
                        run_id: Some(run_id.clone()),
                        error: "Max agent iterations reached".to_string(),
                    },
                )
                .await;
                return;
            }

            let assistant_msg = match serde_json::to_value(&tool_calls) {
                Ok(tc_json) => Message::new(conv_id, Role::Assistant, full_content.clone())
                    .with_tool_calls(tc_json),
                Err(_) => Message::new(conv_id, Role::Assistant, full_content.clone()),
            };
            let _ = persist_message(state, &assistant_msg).await;
            messages.push(assistant_msg);

            let prepared_calls = prepare_tool_calls_for_execution(&tool_calls, conv_id);

            let mut join_set = tokio::task::JoinSet::new();
            for (idx, call) in prepared_calls.into_iter().enumerate() {
                emit_stream_event(
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
                let registry = state.registry.clone();
                let started_event = ServerEvent::ToolStarted {
                    conversation_id: conv_id,
                    run_id: run_id.clone(),
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                };
                emit_stream_event(Some(&event_tx), started_event).await;
                join_set.spawn(async move {
                    let result = registry.execute(&call).await;
                    (idx, call, result)
                });
            }

            let mut results: Vec<(usize, jossie_core::ToolCall, jossie_core::ToolResult)> =
                Vec::with_capacity(tool_calls.len());
            while let Some(res) = join_set.join_next().await {
                if state.is_cancel_requested(conv_id).await {
                    join_set.abort_all();
                    let _ =
                        ensure_run_not_cancelled(state, conv_id, &run_id, Some(&event_tx)).await;
                    return;
                }
                match res {
                    Ok(tuple) => results.push(tuple),
                    Err(e) => tracing::error!("Tool task panicked: {e}"),
                }
            }
            results.sort_by_key(|(idx, _, _)| *idx);

            for (_, call, result) in results {
                emit_stream_event(
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
                let tool_msg = Message::new(conv_id, Role::Tool, result.content)
                    .with_tool_call_id(call.id.clone())
                    .with_name(call.name.clone());
                let _ = persist_message(state, &tool_msg).await;
                messages.push(tool_msg);
            }
            goal_tracker.record_tool_calls(&tool_calls);
            continue;
        }

        if reflection_retries_remaining > 0 {
            if let Some(feedback) = self_reflect(state, &last_user_msg, &full_content).await {
                reflection_retries_remaining -= 1;
                emit_stream_event(
                    Some(&event_tx),
                    ServerEvent::ReflectionRetry {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                        feedback: feedback.clone(),
                    },
                )
                .await;
                emit_stream_event(
                    Some(&event_tx),
                    ServerEvent::AssistantReset {
                        conversation_id: conv_id,
                        run_id: run_id.clone(),
                        reason: "reflection_retry".to_string(),
                    },
                )
                .await;
                messages.push(Message::transient(Role::Assistant, full_content));
                messages.push(Message::transient(
                    Role::System,
                    format!(
                        "[SELF-REFLECTION FEEDBACK: Your response needs improvement. {}. Please revise your response.]",
                        feedback
                    ),
                ));
                continue;
            }
        }

        let assistant_msg = Message::new(conv_id, Role::Assistant, full_content);
        let assistant_reply = assistant_msg.content.clone();
        let user_for_extraction = last_user_msg.clone();
        let _ = persist_message(state, &assistant_msg).await;

        let db = state.db.clone();
        let kg_llm = state.kg_llm.clone();
        tokio::spawn(async move {
            spawn_knowledge_extraction(db, kg_llm, user_for_extraction, assistant_reply).await;
        });

        emit_stream_event(
            Some(&event_tx),
            ServerEvent::RunCompleted {
                conversation_id: conv_id,
                run_id: run_id.clone(),
            },
        )
        .await;
        return;
    }

    emit_stream_event(
        Some(&event_tx),
        ServerEvent::Error {
            conversation_id: conv_id,
            run_id: Some(run_id),
            error: format!(
                "Agent loop exceeded maximum of {} iterations",
                state.max_agent_iterations
            ),
        },
    )
    .await;
}

/// Self-reflection: evaluate response quality using kg_llm.
/// Returns Some(feedback) if the response should be retried, None if it's acceptable.
async fn self_reflect(
    state: &AppState,
    user_message: &str,
    assistant_response: &str,
) -> Option<String> {
    let prompt = format!(
        r#"Evaluate the quality of this assistant response to the user's message.

User message: {user_message}

Assistant response: {assistant_response}

Evaluate on these criteria:
1. Does it actually answer the user's question/request?
2. Is information accurate and complete?
3. Is the tone appropriate?

Respond with EXACTLY one of:
- "PASS" if the response is acceptable
- "RETRY: <specific feedback>" if the response needs improvement

Output only PASS or RETRY: <feedback>, nothing else."#
    );

    let sys = Message::transient(
        Role::System,
        "You are a response quality evaluator. Be concise.".to_string(),
    );
    let user = Message::transient(Role::User, prompt);

    match state.kg_llm.complete(&[sys, user], &[]).await {
        Ok((verdict, _)) => {
            let trimmed = verdict.trim();
            if trimmed.starts_with("RETRY:") {
                let feedback = trimmed.strip_prefix("RETRY:").unwrap_or("").trim();
                tracing::info!("Self-reflection: retry recommended. Feedback: {feedback}");
                Some(feedback.to_string())
            } else {
                tracing::debug!("Self-reflection: response passed quality check");
                None
            }
        }
        Err(e) => {
            tracing::warn!("Self-reflection failed: {e}. Proceeding with original response.");
            None
        }
    }
}

/// Context compression: summarize older messages when context exceeds threshold.
/// Keeps the most recent `keep_recent` messages in full and replaces older ones
/// with a compact summary generated by kg_llm.
const CONTEXT_CHAR_THRESHOLD: usize = 300_000;
const KEEP_RECENT_MESSAGES: usize = 10;

async fn maybe_summarize_context(state: &AppState, conv_id: Uuid, messages: &mut Vec<Message>) {
    let total_chars: usize = messages.iter().map(|m| m.content.len()).sum();
    if total_chars < CONTEXT_CHAR_THRESHOLD || messages.len() <= KEEP_RECENT_MESSAGES {
        return;
    }

    tracing::info!(
        "Context size ({} chars, {} messages) exceeds threshold. Attempting summarization.",
        total_chars,
        messages.len()
    );

    // Check if we already have a recent summary
    if let Ok(Some(existing)) = state.db.get_conversation_summary(conv_id).await {
        // If we already summarized most messages, just use the existing summary
        let unsummarized = messages.len() as i64 - existing.messages_summarized;
        if unsummarized <= KEEP_RECENT_MESSAGES as i64 + 5 {
            // Inject existing summary and keep only recent messages
            let keep_from = messages.len().saturating_sub(KEEP_RECENT_MESSAGES);
            let mut recent = messages.split_off(keep_from);
            sanitize_context_window(&mut recent);
            messages.clear();
            messages.push(Message::transient(
                Role::System,
                format!(
                    "## Conversation Summary (previous {} messages)\n{}",
                    existing.messages_summarized, existing.summary
                ),
            ));
            messages.extend(recent);
            return;
        }
    }

    // Build the older messages to summarize
    let keep_from = messages.len().saturating_sub(KEEP_RECENT_MESSAGES);
    let to_summarize: Vec<String> = messages[..keep_from]
        .iter()
        .map(|m| format!("{:?}: {}", m.role, &m.content[..m.content.len().min(500)]))
        .collect();

    if to_summarize.is_empty() {
        return;
    }

    let summarize_text = to_summarize.join("\n---\n");
    let prompt = format!(
        r#"Summarize the following conversation history into a compact summary.
Preserve: key facts, decisions made, tool results, ongoing goals, and any commitments.
Omit: pleasantries, redundant information, and tool call arguments.
Be concise but complete.

Conversation:
{summarize_text}"#
    );

    let sys = Message::transient(
        Role::System,
        "You are a conversation summarizer. Output a concise summary.".to_string(),
    );
    let user = Message::transient(Role::User, prompt);

    match state.kg_llm.complete(&[sys, user], &[]).await {
        Ok((summary, _)) => {
            let messages_count = keep_from as i64;
            let last_id = messages
                .get(keep_from.saturating_sub(1))
                .map(|m| m.id.to_string());
            let _ = state
                .db
                .save_conversation_summary(conv_id, &summary, messages_count, last_id.as_deref())
                .await;

            let mut recent = messages.split_off(keep_from);
            sanitize_context_window(&mut recent);
            messages.clear();
            messages.push(Message::transient(
                Role::System,
                format!(
                    "## Conversation Summary (previous {} messages)\n{}",
                    messages_count, summary
                ),
            ));
            messages.extend(recent);

            tracing::info!(
                "Summarized {} messages into {} chars. New context: {} messages.",
                messages_count,
                summary.len(),
                messages.len()
            );
        }
        Err(e) => {
            tracing::warn!("Failed to summarize context: {e}. Continuing with full context.");
        }
    }
}

pub(crate) async fn spawn_knowledge_extraction(
    db: Arc<jossie_db::Database>,
    kg_llm: jossie_llm::LlmClient,
    user_msg: String,
    assistant_msg: String,
) {
    if user_msg.len() < 10 && assistant_msg.len() < 10 {
        return; // Skip short chat
    }

    let prompt = format!(
        r#"Extract knowledge from this conversation turn.
Identify Entities (people, projects, concepts) and Relationships.
Ignore trivial chit-chat.

User: {user_msg}
Assistant: {assistant_msg}

Output ONLY valid JSON matching this structure:
{{
  "nodes": [
    {{ "id": "unique_id_lowercase", "label": "Display Name", "type": "Category" }}
  ],
  "edges": [
    {{ "source": "id_source", "target": "id_target", "relation": "RELATION_TYPE" }}
  ]
}}
If nothing to extract, output {{ "nodes": [], "edges": [] }}"#
    );

    let sys_msg = Message::transient(
        Role::System,
        "You are a Knowledge Graph Extractor. Output strictly JSON.".to_string(),
    );
    let user_msg = Message::transient(Role::User, prompt);

    match kg_llm.complete(&[sys_msg, user_msg], &[]).await {
        Ok((response, _)) => {
            let clean_json = response
                .trim()
                .trim_start_matches("```json")
                .trim_end_matches("```");
            match serde_json::from_str::<ExtractionResult>(clean_json) {
                Ok(data) => {
                    let node_count = data.nodes.len();
                    let edge_count = data.edges.len();
                    for node in &data.nodes {
                        let _ = db
                            .graph_upsert_node(
                                &node.id,
                                &node.label,
                                &node.node_type,
                                &serde_json::json!({}),
                            )
                            .await;
                    }
                    for edge in &data.edges {
                        let _ = db
                            .graph_upsert_edge(
                                &edge.source,
                                &edge.target,
                                &edge.relation,
                                1.0,
                                &serde_json::json!({}),
                            )
                            .await;
                    }
                    if node_count > 0 || edge_count > 0 {
                        tracing::info!("Extracted {} nodes and {} edges", node_count, edge_count);
                    }
                }
                Err(e) => tracing::warn!("Failed to parse KG extraction JSON: {e}"),
            }
        }
        Err(e) => tracing::error!("KG Extraction LLM failed: {e}"),
    }
}

pub async fn generate_event_message(
    state: &AppState,
    conversation_id: Uuid,
    event: &IntegrationEvent,
) -> anyhow::Result<Option<String>> {
    {
        let mut active = state.active_conversations.write().await;
        if !active.insert(conversation_id) {
            anyhow::bail!(
                "Conversation {} is already being processed",
                conversation_id
            );
        }
    }

    let result = generate_event_message_inner(state, conversation_id, event).await;

    {
        let mut active = state.active_conversations.write().await;
        active.remove(&conversation_id);
    }

    result
}

#[derive(Debug, Deserialize)]
struct EventModeResponse {
    action: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct EventNotificationState {
    fingerprint: String,
    sent_at: String,
}

const EVENT_NOTIFICATION_MARKER: &str = "integration_event_notification";
const EVENT_MODE_SETTINGS_NAMESPACE: &str = "event_mode";
const EVENT_NOTIFY_COOLDOWN_SECONDS: i64 = 120;

async fn generate_event_message_inner(
    state: &AppState,
    conversation_id: Uuid,
    event: &IntegrationEvent,
) -> anyhow::Result<Option<String>> {
    let mut messages = state
        .db
        .get_messages(conversation_id, Some(state.event_max_context_messages))
        .await?;
    messages.retain(|m| !is_event_notification_message(m));
    strip_tool_activity_from_event_context(&mut messages);

    sanitize_context_window(&mut messages);

    let event_context = build_event_context_hint(event);
    let mut prompt = build_system_prompt(state, Some(&event_context)).await;
    prompt.push_str(
        "\n\n## Event Mode\nYou are receiving a new integration event.\nInterpret this event independently as a fresh arrival.\nDo NOT imply that you made a prior mistake, correction, or retraction unless the event payload explicitly says so.\nFor `gmail_new_message` and `new_email_batch`, frame updates as newly arrived emails, even when similar to prior ones.\nRespond with strict JSON only:\n{\"action\":\"notify\",\"message\":\"<short user-facing message>\"}\nor\n{\"action\":\"skip\",\"message\":\"\"}"
    );

    messages.insert(0, Message::transient(Role::System, prompt));

    let event_payload = serde_json::json!({
        "integration": event.integration,
        "type": event.event_type,
        "payload": event.payload,
        "created_at": event.created_at,
    });

    messages.push(Message::transient(
        Role::User,
        format!(
            "New integration event to evaluate:\n{}",
            serde_json::to_string_pretty(&event_payload)?
        ),
    ));

    let (content, tool_calls) = state.llm.complete(&messages, &[]).await?;
    if !tool_calls.is_empty() {
        tracing::warn!("Event loop returned tool calls; ignoring for now");
    }

    let fingerprint = event_notification_fingerprint(event);

    if let Some(decision) = parse_event_mode_response(&content) {
        let action = decision.action.trim().to_ascii_lowercase();
        if action == "skip" {
            return Ok(None);
        }
        if action == "notify" {
            let message = decision.message.trim();
            if message.is_empty() {
                return Ok(None);
            }
            if should_suppress_event_notification(state, conversation_id, &fingerprint).await? {
                tracing::debug!(
                    "Suppressing duplicate event notification for conversation {}",
                    conversation_id
                );
                return Ok(None);
            }
            record_event_notification(state, conversation_id, &fingerprint).await?;
            return Ok(Some(message.to_string()));
        }
    }

    let trimmed = content.trim();
    if trimmed
        .trim_matches(|c| c == '"' || c == '`')
        .trim()
        .eq_ignore_ascii_case("no_action")
        || trimmed.is_empty()
    {
        return Ok(None);
    }

    tracing::warn!(
        "Dropping invalid event-mode output instead of forwarding raw content: {:.400}",
        trimmed
    );
    Ok(None)
}

fn is_event_notification_message(message: &Message) -> bool {
    message.role == Role::Assistant && message.name.as_deref() == Some(EVENT_NOTIFICATION_MARKER)
}

fn strip_tool_activity_from_event_context(messages: &mut Vec<Message>) {
    messages.retain(|message| message.role != Role::Tool);
    for message in messages.iter_mut() {
        if message.role == Role::Assistant {
            message.tool_calls = None;
            message.tool_call_id = None;
        }
    }
}

fn parse_event_mode_response(content: &str) -> Option<EventModeResponse> {
    let normalized = strip_code_fence(content);
    serde_json::from_str::<EventModeResponse>(normalized)
        .ok()
        .or_else(|| extract_embedded_event_mode_response(normalized))
}

fn strip_code_fence(content: &str) -> &str {
    let trimmed = content.trim();
    if trimmed.starts_with("```") {
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    }
}

fn extract_embedded_event_mode_response(content: &str) -> Option<EventModeResponse> {
    let mut in_string = false;
    let mut escape = false;
    let mut depth = 0usize;
    let mut object_start = None;
    let mut last_match = None;

    for (idx, ch) in content.char_indices() {
        if escape {
            escape = false;
            continue;
        }

        match ch {
            '\\' if in_string => {
                escape = true;
            }
            '"' => {
                in_string = !in_string;
            }
            '{' if !in_string => {
                if depth == 0 {
                    object_start = Some(idx);
                }
                depth += 1;
            }
            '}' if !in_string && depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = object_start.take() {
                        let candidate = &content[start..=idx];
                        if let Ok(parsed) = serde_json::from_str::<EventModeResponse>(candidate) {
                            last_match = Some(parsed);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    last_match
}

fn build_event_context_hint(event: &IntegrationEvent) -> String {
    let mut lines = vec![
        format!("Event type: {}", event.event_type),
        format!("Integration: {}", event.integration),
    ];

    match event.event_type.as_str() {
        "gmail_new_message" | "new_email" => {
            if let Some(from) = event.payload.get("from").and_then(|v| v.as_str()) {
                lines.push(format!("Sender: {}", from));
            }
            if let Some(subject) = event.payload.get("subject").and_then(|v| v.as_str()) {
                lines.push(format!("Subject: {}", subject));
            }
        }
        "new_email_batch" => {
            if let Some(emails) = event.payload.get("emails").and_then(|v| v.as_array()) {
                lines.push(format!("Email count: {}", emails.len()));
                let mut subjects = Vec::new();
                for email in emails.iter().take(5) {
                    if let Some(subject) = email
                        .get("payload")
                        .and_then(|p| p.get("subject"))
                        .and_then(|v| v.as_str())
                    {
                        if !subject.trim().is_empty() {
                            subjects.push(subject.trim().to_string());
                        }
                    }
                }
                if !subjects.is_empty() {
                    lines.push(format!("Subjects: {}", subjects.join(" | ")));
                }
            }
        }
        _ => {}
    }

    lines.join("\n")
}

fn event_notification_fingerprint(event: &IntegrationEvent) -> String {
    match event.event_type.as_str() {
        "gmail_new_message" | "new_email" => {
            let message_id = event
                .payload
                .get("message_unique_id")
                .or_else(|| event.payload.get("message_id"))
                .and_then(|v| v.as_str())
                .unwrap_or(event.dedupe_key.as_str());
            format!(
                "{}|{}|{}|{}",
                event.integration, event.account_id, event.event_type, message_id
            )
        }
        "new_email_batch" => {
            let mut ids: Vec<String> = event
                .payload
                .get("emails")
                .and_then(|v| v.as_array())
                .map(|emails| {
                    emails
                        .iter()
                        .filter_map(|email_event| {
                            let payload = email_event.get("payload")?;
                            payload
                                .get("message_unique_id")
                                .or_else(|| payload.get("message_id"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .or_else(|| {
                                    email_event
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string())
                                })
                        })
                        .collect()
                })
                .unwrap_or_default();
            ids.sort();
            ids.dedup();
            format!(
                "{}|{}|{}|{}",
                event.integration,
                event.account_id,
                event.event_type,
                ids.join(",")
            )
        }
        _ => format!(
            "{}|{}|{}|{}",
            event.integration, event.account_id, event.event_type, event.dedupe_key
        ),
    }
}

async fn should_suppress_event_notification(
    state: &AppState,
    conversation_id: Uuid,
    fingerprint: &str,
) -> anyhow::Result<bool> {
    let key = format!("last_notification:{}", conversation_id);
    let Some(raw) = state
        .db
        .get_integration_setting(EVENT_MODE_SETTINGS_NAMESPACE, &key)
        .await?
    else {
        return Ok(false);
    };

    let Ok(last_state) = serde_json::from_str::<EventNotificationState>(&raw) else {
        return Ok(false);
    };
    if last_state.fingerprint != fingerprint {
        return Ok(false);
    }

    let Ok(last_sent) = chrono::DateTime::parse_from_rfc3339(&last_state.sent_at) else {
        return Ok(false);
    };
    let elapsed = chrono::Utc::now() - last_sent.with_timezone(&chrono::Utc);
    Ok(elapsed < chrono::Duration::seconds(EVENT_NOTIFY_COOLDOWN_SECONDS))
}

async fn record_event_notification(
    state: &AppState,
    conversation_id: Uuid,
    fingerprint: &str,
) -> anyhow::Result<()> {
    let key = format!("last_notification:{}", conversation_id);
    let state_value = EventNotificationState {
        fingerprint: fingerprint.to_string(),
        sent_at: chrono::Utc::now().to_rfc3339(),
    };
    state
        .db
        .set_integration_setting(
            EVENT_MODE_SETTINGS_NAMESPACE,
            &key,
            &serde_json::to_string(&state_value)?,
        )
        .await
}

async fn build_graph_context(state: &AppState, user_message: &str) -> String {
    if user_message.trim().len() < 3 {
        return String::new();
    }

    let mut candidates = extract_candidate_entities(user_message);

    // NEW: Enrich candidates with context-aware searches
    candidates = enrich_candidates_with_context(state, user_message, candidates).await;

    if candidates.is_empty() {
        return String::new();
    }

    const MAX_CANDIDATES: usize = 8;
    const MAX_NODES: usize = 6;
    const MAX_EDGES_PER_NODE: usize = 6;

    let mut nodes = Vec::new();
    let mut seen_nodes = HashSet::new();

    for candidate in candidates.into_iter().take(MAX_CANDIDATES) {
        let Ok(found) = state.db.graph_find_nodes(&candidate).await else {
            continue;
        };
        for node in found {
            if seen_nodes.insert(node.id.clone()) {
                nodes.push(node);
            }
            if nodes.len() >= MAX_NODES {
                break;
            }
        }
        if nodes.len() >= MAX_NODES {
            break;
        }
    }

    if nodes.is_empty() {
        return String::new();
    }

    let mut lines = Vec::new();
    lines.push("## Context Graph".to_string());

    let mut seen_edges = HashSet::new();
    for node in &nodes {
        lines.push(format!("- {} [{}]", node.label, node.node_type));

        let Ok(neighbors) = state.db.graph_get_neighbors(&node.id).await else {
            continue;
        };
        for neighbor in neighbors.into_iter().take(MAX_EDGES_PER_NODE) {
            let line = if neighbor.direction == "outgoing" {
                format!(
                    "- {} --[{}]--> {}",
                    node.label, neighbor.relation, neighbor.node.label
                )
            } else {
                format!(
                    "- {} --[{}]--> {}",
                    neighbor.node.label, neighbor.relation, node.label
                )
            };
            if seen_edges.insert(line.clone()) {
                lines.push(line);
            }
        }
    }

    let mut context = lines.join("\n");

    // NEW: Add contextual hints to encourage proactive searching
    let lower_msg = user_message.to_lowercase();

    if user_message.contains('?') && nodes.len() < 3 {
        context.push_str("\n\n**Hint**: This is a question. Consider using graph_search to find more relevant context before answering.");
    }

    if (lower_msg.contains("work") || lower_msg.contains("project"))
        && !nodes.iter().any(|n| n.node_type == "Project")
    {
        context.push_str("\n\n**Hint**: Work/project mentioned. Use graph_list_by_type to find relevant Project entities.");
    }

    if (lower_msg.contains("who") || lower_msg.contains("people"))
        && !nodes.iter().any(|n| n.node_type == "Person")
    {
        context.push_str("\n\n**Hint**: Question about people. Use graph_list_by_type('Person') to see all known individuals.");
    }

    context
}

/// Enrich entity candidates with context-aware graph searches
async fn enrich_candidates_with_context(
    state: &AppState,
    message: &str,
    mut candidates: Vec<String>,
) -> Vec<String> {
    let lower = message.to_lowercase();

    // Proactively search for entities when certain keywords appear
    if lower.contains("work") || lower.contains("project") || lower.contains("job") {
        if let Ok(nodes) = state.db.graph_list_nodes_by_type("Project").await {
            candidates.extend(nodes.into_iter().map(|n| n.label).take(3));
        }
    }

    if lower.contains("meeting")
        || lower.contains("talk")
        || lower.contains("discuss")
        || lower.contains("call")
    {
        if let Ok(nodes) = state.db.graph_list_nodes_by_type("Person").await {
            candidates.extend(nodes.into_iter().map(|n| n.label).take(5));
        }
    }

    if lower.contains("company") || lower.contains("organization") {
        if let Ok(nodes) = state.db.graph_list_nodes_by_type("Company").await {
            candidates.extend(nodes.into_iter().map(|n| n.label).take(3));
        }
    }

    // Add frequently mentioned entities from memory (if stored)
    if let Ok(Some(freq_entities)) = state.db.get_memory("frequent_entities").await {
        if let Ok(entities) = serde_json::from_str::<Vec<String>>(&freq_entities.content) {
            candidates.extend(entities.into_iter().take(3));
        }
    }

    // Deduplicate while preserving order
    let mut seen = HashSet::new();
    candidates.retain(|c| seen.insert(c.to_lowercase()));

    candidates
}

fn extract_candidate_entities(message: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    // Extract quoted phrases (existing logic)
    for quoted in extract_quoted_phrases(message) {
        let key = quoted.to_lowercase();
        if seen.insert(key) {
            candidates.push(quoted);
        }
    }

    // NEW: Extract email addresses as entity candidates
    if let Ok(email_regex) = Regex::new(r"\b([a-zA-Z0-9._-]+)@([a-zA-Z0-9._-]+\.[a-zA-Z]+)\b") {
        for cap in email_regex.captures_iter(message) {
            if let Some(email) = cap.get(0) {
                let email_str = email.as_str().to_string();
                let key = email_str.to_lowercase();
                if seen.insert(key) {
                    candidates.push(email_str);
                }

                // Also try to extract name from email (e.g., john.doe -> John Doe)
                if let Some(username) = cap.get(1) {
                    let name_parts: Vec<&str> = username.as_str().split(&['.', '_', '-']).collect();
                    if name_parts.len() >= 2 {
                        let formatted_name: String = name_parts
                            .iter()
                            .map(|part| {
                                let mut chars = part.chars();
                                match chars.next() {
                                    None => String::new(),
                                    Some(first) => {
                                        first.to_uppercase().collect::<String>()
                                            + &chars.as_str().to_lowercase()
                                    }
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" ");

                        let key = formatted_name.to_lowercase();
                        if seen.insert(key) {
                            candidates.push(formatted_name);
                        }
                    }
                }
            }
        }
    }

    // NEW: Extract @mentions (social media style)
    if let Ok(mention_regex) = Regex::new(r"@([A-Za-z0-9_]+)") {
        for cap in mention_regex.captures_iter(message) {
            if let Some(mention) = cap.get(1) {
                let mention_str = mention.as_str().to_string();
                let key = mention_str.to_lowercase();
                if seen.insert(key) {
                    candidates.push(mention_str);
                }
            }
        }
    }

    // NEW: Extract role-based names (e.g., "my boss Alice", "colleague Bob")
    if let Ok(role_regex) = Regex::new(
        r"(?i)\b(?:my|our|the)\s+(boss|manager|colleague|friend|partner|coworker|supervisor|assistant|teammate)\s+([A-Z][a-z]+(?:\s+[A-Z][a-z]+)*)\b",
    ) {
        for cap in role_regex.captures_iter(message) {
            if let Some(name) = cap.get(2) {
                let name_str = name.as_str().to_string();
                let key = name_str.to_lowercase();
                if seen.insert(key) {
                    candidates.push(name_str);
                }
            }
        }
    }

    // Existing: Extract capitalized token sequences
    let stopwords: HashSet<&'static str> = [
        "the", "a", "an", "and", "or", "but", "to", "from", "in", "on", "at", "for", "with", "of",
        "my", "your", "our", "his", "her", "their", "it", "this", "that", "i", "we", "you", "he",
        "she", "they",
    ]
    .into_iter()
    .collect();

    let mut current = Vec::new();
    for raw_token in message.split_whitespace() {
        let token = raw_token.trim_matches(|c: char| c.is_ascii_punctuation());
        if token.is_empty() {
            continue;
        }

        if is_entity_token(token, &stopwords) {
            current.push(token.to_string());
        } else if !current.is_empty() {
            let phrase = current.join(" ");
            let key = phrase.to_lowercase();
            if seen.insert(key) {
                candidates.push(phrase);
            }
            current.clear();
        }
    }

    if !current.is_empty() {
        let phrase = current.join(" ");
        let key = phrase.to_lowercase();
        if seen.insert(key) {
            candidates.push(phrase);
        }
    }

    candidates
}

fn extract_quoted_phrases(message: &str) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in message.chars() {
        match ch {
            '"' => {
                if in_quotes {
                    let trimmed = current.trim();
                    if trimmed.len() >= 2 {
                        phrases.push(trimmed.to_string());
                    }
                    current.clear();
                    in_quotes = false;
                } else {
                    current.clear();
                    in_quotes = true;
                }
            }
            _ => {
                if in_quotes {
                    current.push(ch);
                }
            }
        }
    }

    phrases
}

fn is_entity_token(token: &str, stopwords: &HashSet<&str>) -> bool {
    let lower = token.to_lowercase();
    if stopwords.contains(lower.as_str()) {
        return false;
    }

    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }

    token.len() > 1
}

fn sanitize_context_window(messages: &mut Vec<Message>) {
    let mut sanitized = Vec::with_capacity(messages.len());
    let mut idx = 0usize;

    while idx < messages.len() {
        let message = &messages[idx];

        if message.role == Role::Tool {
            tracing::warn!("Sanitizing context window: removing orphaned tool message");
            idx += 1;
            continue;
        }

        if message.role == Role::Assistant {
            if let Some(tool_calls_value) = &message.tool_calls {
                let mut block_end = idx + 1;
                let mut matched_call_ids = std::collections::HashSet::new();
                while block_end < messages.len() && messages[block_end].role == Role::Tool {
                    if let Some(call_id) = &messages[block_end].tool_call_id {
                        matched_call_ids.insert(call_id.clone());
                    }
                    block_end += 1;
                }

                let expected_call_ids: Vec<String> = match serde_json::from_value::<
                    Vec<jossie_core::ToolCall>,
                >(
                    tool_calls_value.clone()
                ) {
                    Ok(calls) => calls.into_iter().map(|call| call.id).collect(),
                    Err(err) => {
                        tracing::warn!(
                            "Sanitizing context window: removing assistant tool-call block with invalid tool_calls payload: {err}"
                        );
                        Vec::new()
                    }
                };

                let has_all_outputs = !expected_call_ids.is_empty()
                    && expected_call_ids
                        .iter()
                        .all(|call_id| matched_call_ids.contains(call_id));

                if has_all_outputs {
                    sanitized.extend(messages[idx..block_end].iter().cloned());
                } else {
                    tracing::warn!(
                        "Sanitizing context window: removing assistant tool-call block with {} trailing tool message(s)",
                        block_end.saturating_sub(idx + 1)
                    );
                }

                idx = block_end;
                continue;
            }
        }

        sanitized.push(message.clone());
        idx += 1;
    }

    if sanitized.len() != messages.len() {
        *messages = sanitized;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_msg(role: Role) -> Message {
        Message::new(Uuid::new_v4(), role, "test".to_string())
    }

    #[test]
    fn test_sanitize_removes_orphan_tool() {
        let mut msgs = vec![make_msg(Role::Tool), make_msg(Role::User)];
        sanitize_context_window(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[test]
    fn test_sanitize_preserves_valid_history() {
        let mut msgs = vec![make_msg(Role::User), make_msg(Role::Assistant)];
        sanitize_context_window(&mut msgs);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn test_sanitize_removes_multiple_orphans() {
        let mut msgs = vec![
            make_msg(Role::Tool),
            make_msg(Role::Tool),
            make_msg(Role::Assistant),
        ];
        sanitize_context_window(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::Assistant);
    }

    #[test]
    fn test_sanitize_removes_orphan_assistant_tool_call_block() {
        let conv_id = Uuid::new_v4();
        let assistant = Message::new(conv_id, Role::Assistant, String::new()).with_tool_calls(
            serde_json::json!([{
                "id": "call_123",
                "name": "lookup",
                "arguments": "{}"
            }]),
        );
        let user = Message::new(conv_id, Role::User, "next".to_string());

        let mut msgs = vec![assistant, user];
        sanitize_context_window(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[test]
    fn test_sanitize_preserves_assistant_tool_call_with_outputs() {
        let conv_id = Uuid::new_v4();
        let assistant = Message::new(conv_id, Role::Assistant, String::new()).with_tool_calls(
            serde_json::json!([{
                "id": "call_123",
                "name": "lookup",
                "arguments": "{}"
            }]),
        );
        let tool = Message::new(conv_id, Role::Tool, "ok".to_string())
            .with_tool_call_id("call_123".to_string());
        let user = Message::new(conv_id, Role::User, "next".to_string());

        let mut msgs = vec![assistant, tool, user];
        sanitize_context_window(&mut msgs);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, Role::Assistant);
        assert_eq!(msgs[1].role, Role::Tool);
    }

    #[test]
    fn test_sanitize_removes_trailing_assistant_tool_call_block() {
        let conv_id = Uuid::new_v4();
        let user = Message::new(conv_id, Role::User, "hello".to_string());
        let assistant = Message::new(conv_id, Role::Assistant, String::new()).with_tool_calls(
            serde_json::json!([{
                "id": "call_123",
                "name": "lookup",
                "arguments": "{}"
            }]),
        );

        let mut msgs = vec![user, assistant];
        sanitize_context_window(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[test]
    fn test_memory_index_lists_all_keys() {
        let keys = vec![
            MemoryKeyInfo {
                key: "user_profile.location".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-02T00:00:00Z".to_string(),
            },
            MemoryKeyInfo {
                key: "agent_profile.mood".to_string(),
                created_at: "2026-01-03T00:00:00Z".to_string(),
                updated_at: "2026-01-04T00:00:00Z".to_string(),
            },
        ];

        let section = format_memory_index(&keys);
        assert!(section.contains("user_profile.location"));
        assert!(section.contains("agent_profile.mood"));
        assert!(section.contains("2026-01-02T00:00:00Z"));
        assert!(section.contains("2026-01-04T00:00:00Z"));
    }

    #[test]
    fn test_memory_index_empty_state() {
        let section = format_memory_index(&[]);
        assert!(section.contains("No memories are currently saved"));
    }

    #[test]
    fn test_parse_event_mode_response_extracts_embedded_json() {
        let content = r#"to=multi_tool_use.parallel blah
{"tool_uses":[{"recipient_name":"functions.gmail_read","parameters":{"message_id":"abc"}}]}
{"action":"notify","message":"Two transaction emails just came in."}"#;

        let parsed = parse_event_mode_response(content).expect("expected parsed response");
        assert_eq!(parsed.action, "notify");
        assert_eq!(parsed.message, "Two transaction emails just came in.");
    }

    #[test]
    fn test_parse_event_mode_response_rejects_non_json_text() {
        assert!(parse_event_mode_response("let me check those emails first").is_none());
    }

    #[test]
    fn test_prepare_tool_calls_injects_conversation_id_for_scheduler_tools() {
        let conv_id = Uuid::new_v4();
        let calls = vec![
            jossie_core::ToolCall {
                id: "call_1".to_string(),
                name: "schedule_task".to_string(),
                arguments: r#"{"prompt":"check in","run_at":"2026-04-01T12:00:00Z"}"#.to_string(),
            },
            jossie_core::ToolCall {
                id: "call_2".to_string(),
                name: "memory_search".to_string(),
                arguments: r#"{"query":"hi"}"#.to_string(),
            },
        ];

        let prepared = prepare_tool_calls_for_execution(&calls, conv_id);
        let scheduler_args: serde_json::Value =
            serde_json::from_str(&prepared[0].arguments).expect("scheduler args should be JSON");

        assert_eq!(
            scheduler_args["__conversation_id"],
            serde_json::Value::String(conv_id.to_string())
        );
        assert_eq!(prepared[1].arguments, calls[1].arguments);
    }
}

#[derive(Deserialize)]
struct ExtractionResult {
    #[serde(default)]
    nodes: Vec<ExtractedNode>,
    #[serde(default)]
    edges: Vec<ExtractedEdge>,
}

#[derive(Deserialize)]
struct ExtractedNode {
    id: String,
    label: String,
    #[serde(rename = "type")]
    node_type: String,
}

#[derive(Deserialize)]
struct ExtractedEdge {
    source: String,
    target: String,
    relation: String,
}
