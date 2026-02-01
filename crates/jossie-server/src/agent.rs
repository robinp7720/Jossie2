use crate::state::AppState;
use chrono::Utc;
use jossie_core::types::{Message, Role};
use jossie_db::IntegrationEvent;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

async fn build_system_prompt(state: &AppState, user_message: Option<&str>) -> String {
    let mut prompt = state.system_prompt.clone();

    // Dynamically append agent and user profiles from memory
    if let Ok(Some(entry)) = state.db.get_memory("agent_profile").await {
        prompt.push_str("\n\n## Agent Description (Jossie)\n");
        prompt.push_str(&entry.content);
    }

    if let Ok(Some(entry)) = state.db.get_memory("user_profile").await {
        prompt.push_str("\n\n## User Description\n");
        prompt.push_str(&entry.content);
    }

    if let Some(message) = user_message {
        let graph_context = build_graph_context(state, message).await;
        if !graph_context.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&graph_context);
        }
    }

    prompt
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
    let sys_msg = Message {
        id: Uuid::nil(),
        conversation_id: Uuid::nil(),
        role: Role::System,
        content,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        created_at: Utc::now(),
    };
    messages.insert(0, sys_msg);
}

pub async fn run_agent_loop(state: &AppState, conv_id: Uuid) -> anyhow::Result<String> {
    let tools = state.registry.all_tool_definitions();
    let mut messages = state.db.get_messages(conv_id).await?;

    // Capture user message for extraction later
    let last_user_msg = messages
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default();

    prepend_system_prompt(state, &mut messages, Some(&last_user_msg)).await;

    for _iteration in 0..state.max_agent_iterations {
        let (content, tool_calls) = state.llm.complete(&messages, &tools).await?;

        if tool_calls.is_empty() {
            let msg = Message {
                id: Uuid::new_v4(),
                conversation_id: conv_id,
                role: Role::Assistant,
                content: content.clone(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                created_at: Utc::now(),
            };
            state.db.save_message(&msg).await?;

            // Trigger background extraction
            let db = state.db.clone();
            let llm = state.llm.clone();
            let assistant_reply = content.clone();

            tokio::spawn(async move {
                spawn_knowledge_extraction(db, llm, last_user_msg, assistant_reply).await;
            });

            return Ok(content);
        }

        let tc_json = serde_json::to_value(&tool_calls)?;
        let assistant_msg = Message {
            id: Uuid::new_v4(),
            conversation_id: conv_id,
            role: Role::Assistant,
            content: content.clone(),
            tool_calls: Some(tc_json),
            tool_call_id: None,
            name: None,
            created_at: Utc::now(),
        };
        state.db.save_message(&assistant_msg).await?;
        messages.push(assistant_msg);

        for call in &tool_calls {
            let result = state.registry.execute(call).await;
            let tool_msg = Message {
                id: Uuid::new_v4(),
                conversation_id: conv_id,
                role: Role::Tool,
                content: result.content,
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
                name: Some(call.name.clone()),
                created_at: Utc::now(),
            };
            state.db.save_message(&tool_msg).await?;
            messages.push(tool_msg);
        }
    }

    anyhow::bail!(
        "Agent loop exceeded maximum of {} iterations",
        state.max_agent_iterations
    )
}

pub(crate) async fn spawn_knowledge_extraction(
    db: Arc<jossie_db::Database>,
    llm: jossie_llm::LlmClient,
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

    let sys_msg = Message {
        id: Uuid::nil(),
        conversation_id: Uuid::nil(),
        role: Role::System,
        content: "You are a Knowledge Graph Extractor. Output strictly JSON.".to_string(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        created_at: Utc::now(),
    };
    let user_msg = Message {
        id: Uuid::nil(),
        conversation_id: Uuid::nil(),
        role: Role::User,
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        created_at: Utc::now(),
    };

    match llm.complete(&[sys_msg, user_msg], &[]).await {
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
    let mut messages = state.db.get_messages(conversation_id).await?;

    let mut prompt = build_system_prompt(state, None).await;
    prompt.push_str(
        "\n\n## Event Mode\nYou are receiving integration events.\nDecide whether to proactively message the user.\nIf yes, respond with a short, friendly message as the assistant.\nIf not, respond exactly: NO_ACTION"
    );

    let sys_msg = Message {
        id: Uuid::nil(),
        conversation_id: Uuid::nil(),
        role: Role::System,
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        created_at: Utc::now(),
    };
    messages.insert(0, sys_msg);

    let event_payload = serde_json::json!({
        "integration": event.integration,
        "type": event.event_type,
        "payload": event.payload,
        "created_at": event.created_at,
    });

    let event_msg = Message {
        id: Uuid::nil(),
        conversation_id: Uuid::nil(),
        role: Role::System,
        content: serde_json::to_string_pretty(&event_payload)?,
        tool_calls: None,
        tool_call_id: None,
        name: Some("integration_event".to_string()),
        created_at: Utc::now(),
    };
    messages.push(event_msg);

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

    let candidates = extract_candidate_entities(user_message);
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

    lines.join("\n")
}

fn extract_candidate_entities(message: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for quoted in extract_quoted_phrases(message) {
        let key = quoted.to_lowercase();
        if seen.insert(key) {
            candidates.push(quoted);
        }
    }

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
