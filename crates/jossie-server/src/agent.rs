use crate::state::AppState;
use jossie_core::types::{Message, Role};
use jossie_db::{IntegrationEvent, MemoryKeyInfo};
use regex::Regex;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

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

pub async fn run_agent_loop_with_options(
    state: &AppState,
    conv_id: Uuid,
    options: AgentRunOptions,
) -> anyhow::Result<String> {
    // Try to claim this conversation
    {
        let mut active = state.active_conversations.write().await;
        if !active.insert(conv_id) {
            anyhow::bail!("Conversation {} is already being processed", conv_id);
        }
    }

    // Execute the agent loop and ensure we release the lock even on panic/error
    let result = run_agent_loop_inner(state, conv_id, &options).await;

    // Release the conversation lock
    {
        let mut active = state.active_conversations.write().await;
        active.remove(&conv_id);
    }

    result
}

async fn run_agent_loop_inner(
    state: &AppState,
    conv_id: Uuid,
    options: &AgentRunOptions,
) -> anyhow::Result<String> {
    let mut tools = state.registry.all_tool_definitions();
    if !options.allow_schedule_management {
        tools.retain(|tool| tool.name != "schedule_task" && tool.name != "schedule_recurring_task");
    }
    if !options.allow_oob_messages {
        tools.retain(|tool| tool.name != "send_user_message");
    }

    // Fetch only the relevant context window from DB
    let mut messages = state
        .db
        .get_messages(conv_id, Some(state.max_context_messages))
        .await?;

    // Capture user message for extraction later
    let last_user_msg = messages
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default();

    sanitize_context_window(&mut messages);

    prepend_system_prompt(state, &mut messages, Some(&last_user_msg)).await;
    if options.scheduled_execution {
        messages.insert(1, Message::transient(
            Role::System,
            "Scheduled execution mode: this turn was triggered by an existing schedule. Execute the task now and do not create new schedules unless the user explicitly asks in this same turn.".to_string(),
        ).with_name("scheduled_execution_mode".to_string()));
    }

    for _iteration in 0..state.max_agent_iterations {
        // --- DEBUG: Log Context Size ---
        let total_chars: usize = messages.iter().map(|m| m.content.len()).sum();
        let est_tokens = total_chars / 4; // Rough estimate
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
        // -------------------------------

        let (content, tool_calls) = state.llm.complete(&messages, &tools).await?;

        if tool_calls.is_empty() {
            let msg = Message::new(conv_id, Role::Assistant, content.clone());
            state.db.save_message(&msg).await?;

            // Trigger background extraction
            let db = state.db.clone();
            let kg_llm = state.kg_llm.clone();
            let assistant_reply = content.clone();

            tokio::spawn(async move {
                spawn_knowledge_extraction(db, kg_llm, last_user_msg, assistant_reply).await;
            });

            return Ok(content);
        }

        let tc_json = serde_json::to_value(&tool_calls)?;
        let assistant_msg =
            Message::new(conv_id, Role::Assistant, content.clone()).with_tool_calls(tc_json);
        state.db.save_message(&assistant_msg).await?;
        messages.push(assistant_msg);

        for call in &tool_calls {
            // Inject conversation_id into scheduler tool arguments
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

            tracing::info!(
                "Executing tool: {} with args: {}",
                call.name,
                call.arguments
            );
            let result = state.registry.execute(&call_with_context).await;
            tracing::info!(
                "Tool {} finished. Result preview: {:.200}...",
                call.name,
                result.content
            );
            let tool_msg = Message::new(conv_id, Role::Tool, result.content)
                .with_tool_call_id(call.id.clone())
                .with_name(call.name.clone());
            state.db.save_message(&tool_msg).await?;
            messages.push(tool_msg);
        }
    }

    anyhow::bail!(
        "Agent loop exceeded maximum of {} iterations",
        state.max_agent_iterations
    )
}

/// Events emitted by the streaming agent loop.
#[derive(Debug, Clone)]
pub enum AgentStreamEvent {
    /// A text delta from the LLM response.
    Delta(String),
    /// A tool was executed and produced a result.
    ToolResult { tool: String, result: String },
    /// The agent loop completed for this conversation.
    Done { conversation_id: Uuid },
    /// An error occurred.
    Error(String),
}

/// Run the agent loop with streaming, sending events to the caller via an mpsc channel.
/// This is the streaming counterpart of `run_agent_loop`.
pub async fn run_agent_loop_streaming(
    state: &AppState,
    conv_id: Uuid,
    event_tx: tokio::sync::mpsc::Sender<AgentStreamEvent>,
) {
    let tools = state.registry.all_tool_definitions();
    let mut messages = match state
        .db
        .get_messages(conv_id, Some(state.max_context_messages))
        .await
    {
        Ok(m) => m,
        Err(e) => {
            let _ = event_tx
                .send(AgentStreamEvent::Error(e.to_string()))
                .await;
            return;
        }
    };

    let last_user_msg = messages
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default();

    prepend_system_prompt(state, &mut messages, Some(&last_user_msg)).await;

    for iteration in 0..state.max_agent_iterations {
        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel(100);
        let llm = state.llm.clone();
        let messages_clone = messages.clone();
        let tools_clone = tools.clone();

        tokio::spawn(async move {
            if let Err(e) = llm
                .complete_stream(&messages_clone, &tools_clone, stream_tx)
                .await
            {
                tracing::error!("LLM stream error: {e}");
            }
        });

        let mut full_content = String::new();
        let mut tool_calls = Vec::new();

        while let Some(event) = stream_rx.recv().await {
            match event {
                jossie_llm::StreamEvent::Delta(delta) => {
                    full_content.push_str(&delta);
                    let _ = event_tx.send(AgentStreamEvent::Delta(delta)).await;
                }
                jossie_llm::StreamEvent::ToolCalls(calls) => {
                    tool_calls = calls;
                }
                jossie_llm::StreamEvent::Done => {}
                jossie_llm::StreamEvent::Error(e) => {
                    let _ = event_tx.send(AgentStreamEvent::Error(e)).await;
                }
            }
        }

        if !tool_calls.is_empty() {
            if iteration + 1 >= state.max_agent_iterations {
                let _ = event_tx
                    .send(AgentStreamEvent::Error(
                        "Max agent iterations reached".to_string(),
                    ))
                    .await;
                return;
            }

            let assistant_msg = match serde_json::to_value(&tool_calls) {
                Ok(tc_json) => Message::new(conv_id, Role::Assistant, full_content.clone())
                    .with_tool_calls(tc_json),
                Err(_) => Message::new(conv_id, Role::Assistant, full_content.clone()),
            };
            let _ = state.db.save_message(&assistant_msg).await;
            messages.push(assistant_msg);

            for call in &tool_calls {
                let result = state.registry.execute(call).await;
                let _ = event_tx
                    .send(AgentStreamEvent::ToolResult {
                        tool: call.name.clone(),
                        result: result.content.clone(),
                    })
                    .await;
                let tool_msg = Message::new(conv_id, Role::Tool, result.content)
                    .with_tool_call_id(call.id.clone())
                    .with_name(call.name.clone());
                let _ = state.db.save_message(&tool_msg).await;
                messages.push(tool_msg);
            }
            continue;
        }

        // Final response — save and trigger extraction
        let assistant_msg = Message::new(conv_id, Role::Assistant, full_content);
        let _ = state.db.save_message(&assistant_msg).await;

        let db = state.db.clone();
        let kg_llm = state.kg_llm.clone();
        let assistant_reply = assistant_msg.content.clone();
        let user_for_extraction = last_user_msg.clone();
        tokio::spawn(async move {
            spawn_knowledge_extraction(db, kg_llm, user_for_extraction, assistant_reply).await;
        });

        let _ = event_tx
            .send(AgentStreamEvent::Done {
                conversation_id: conv_id,
            })
            .await;
        return;
    }

    let _ = event_tx
        .send(AgentStreamEvent::Error(format!(
            "Agent loop exceeded maximum of {} iterations",
            state.max_agent_iterations
        )))
        .await;
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
    let mut messages = state
        .db
        .get_messages(conversation_id, Some(state.max_context_messages))
        .await?;

    sanitize_context_window(&mut messages);

    let mut prompt = build_system_prompt(state, None).await;
    prompt.push_str(
        "\n\n## Event Mode\nYou are receiving integration events.\nDecide whether to proactively message the user.\nIf yes, respond with a short, friendly message as the assistant.\nIf not, respond exactly: NO_ACTION"
    );

    messages.insert(0, Message::transient(Role::System, prompt));

    let event_payload = serde_json::json!({
        "integration": event.integration,
        "type": event.event_type,
        "payload": event.payload,
        "created_at": event.created_at,
    });

    messages.push(
        Message::transient(Role::System, serde_json::to_string_pretty(&event_payload)?)
            .with_name("integration_event".to_string()),
    );

    let (content, tool_calls) = state.llm.complete(&messages, &[]).await?;
    if !tool_calls.is_empty() {
        tracing::warn!("Event loop returned tool calls; ignoring for now");
    }

    let trimmed = content.trim();
    let normalized = trimmed.trim_matches(|c| c == '"' || c == '`').trim();
    if normalized.eq_ignore_ascii_case("no_action") || normalized.is_empty() {
        return Ok(None);
    }

    Ok(Some(trimmed.to_string()))
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
    // If the first message is a Tool output, it's orphaned because we lost the Assistant call.
    // We must drain all leading Tool messages until we hit a non-Tool message.
    let mut split_idx = 0;
    for (i, msg) in messages.iter().enumerate() {
        if msg.role == Role::Tool {
            split_idx = i + 1;
        } else {
            break;
        }
    }

    if split_idx > 0 {
        tracing::warn!(
            "Sanitizing context window: removing {} orphaned tool messages",
            split_idx
        );
        messages.drain(0..split_idx);
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
