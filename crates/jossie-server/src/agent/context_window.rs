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

    let lower_message = user_message.to_lowercase();
    let has_context_intent = contains_any(
        &lower_message,
        &[
            "remember",
            "who",
            "person",
            "people",
            "project",
            "work",
            "meeting",
            "company",
            "relationship",
            "context",
            "know about",
        ],
    );
    let first_word = user_message
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|ch: char| ch.is_ascii_punctuation());
    if candidates.len() == 1
        && candidates[0].eq_ignore_ascii_case(first_word)
        && !has_context_intent
    {
        return String::new();
    }

    const MAX_CANDIDATES: usize = 8;
    const MAX_NODES: usize = 6;
    const MAX_EDGES_PER_NODE: usize = 6;

    let candidates = candidates
        .into_iter()
        .take(MAX_CANDIDATES)
        .collect::<Vec<_>>();
    let Ok(nodes) = state.db.graph_find_nodes_many(&candidates, MAX_NODES).await else {
        return String::new();
    };

    if nodes.is_empty() {
        return String::new();
    }

    let mut lines = Vec::new();
    lines.push("## Context Graph".to_string());

    let mut seen_edges = HashSet::new();
    let node_ids = nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
    let mut neighbors_by_node = state
        .db
        .graph_get_neighbors_many(&node_ids, MAX_EDGES_PER_NODE)
        .await
        .unwrap_or_default();
    for node in &nodes {
        lines.push(format!("- {} [{}]", node.label, node.node_type));

        for neighbor in neighbors_by_node.remove(&node.id).unwrap_or_default() {
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
    let lower_msg = lower_message;

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
    let wants_projects =
        lower.contains("work") || lower.contains("project") || lower.contains("job");
    let wants_people = lower.contains("meeting")
        || lower.contains("talk")
        || lower.contains("discuss")
        || lower.contains("call");
    let wants_companies = lower.contains("company") || lower.contains("organization");
    let mut node_types = Vec::new();
    if wants_projects {
        node_types.push("Project");
    }
    if wants_people {
        node_types.push("Person");
    }
    if wants_companies {
        node_types.push("Company");
    }
    let (context_nodes, frequent_entities) = tokio::join!(
        state.db.graph_list_nodes_by_types(&node_types, 15),
        state.db.get_memory("frequent_entities")
    );
    let context_nodes = context_nodes.unwrap_or_default();
    candidates.extend(
        context_nodes
            .iter()
            .filter(|node| node.node_type == "Project")
            .take(3)
            .map(|node| node.label.clone()),
    );
    candidates.extend(
        context_nodes
            .iter()
            .filter(|node| node.node_type == "Person")
            .take(5)
            .map(|node| node.label.clone()),
    );
    candidates.extend(
        context_nodes
            .iter()
            .filter(|node| node.node_type == "Company")
            .take(3)
            .map(|node| node.label.clone()),
    );
    if let Ok(Some(freq_entities)) = frequent_entities
        && let Ok(entities) = serde_json::from_str::<Vec<String>>(&freq_entities.content)
    {
        candidates.extend(entities.into_iter().take(3));
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

fn remove_completed_historical_tool_activity(messages: &mut Vec<Message>) {
    let Some(last_final_assistant) = messages.iter().rposition(|message| {
        message.role == Role::Assistant
            && message.tool_calls.is_none()
            && !message.content.trim().is_empty()
    }) else {
        return;
    };

    let mut projected = Vec::with_capacity(messages.len());
    let mut idx = 0usize;
    let mut removed_chars = 0usize;
    while idx < messages.len() {
        let message = &messages[idx];
        if message.role == Role::Assistant && message.tool_calls.is_some() {
            let mut block_end = idx + 1;
            while block_end < messages.len() && messages[block_end].role == Role::Tool {
                block_end += 1;
            }
            if block_end <= last_final_assistant {
                removed_chars += messages[idx..block_end]
                    .iter()
                    .map(|message| message.content.len())
                    .sum::<usize>();
                idx = block_end;
                continue;
            }
        }
        projected.push(message.clone());
        idx += 1;
    }

    if removed_chars > 0 {
        tracing::debug!(
            removed_chars,
            remaining_messages = projected.len(),
            "Projected completed historical tool activity out of prompt context"
        );
        *messages = projected;
    }
}

fn bound_context_window(
    messages: &mut Vec<Message>,
    max_chars: usize,
    target_chars: usize,
    keep_recent_dialogue: usize,
) {
    if context_chars(messages) <= max_chars {
        return;
    }

    let dialogue_indices = messages
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, message)| matches!(message.role, Role::User | Role::Assistant))
        .take(keep_recent_dialogue)
        .map(|(idx, _)| idx)
        .collect::<HashSet<_>>();
    let first_required = dialogue_indices
        .iter()
        .copied()
        .min()
        .unwrap_or(messages.len());
    let before = context_chars(messages);
    let mut retained = messages
        .iter()
        .enumerate()
        .filter(|(idx, message)| message.role == Role::System || *idx >= first_required)
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    sanitize_context_window(&mut retained);
    *messages = retained;

    // Compact in a finite pass. The former loop used `preview_text`, which adds
    // three characters after truncating. When the context was exactly three
    // characters over target it could therefore repeat forever without making
    // progress. Every candidate below is visited at most once and the marker is
    // included in the requested limit.
    let newest_tool = messages
        .iter()
        .rposition(|message| message.role == Role::Tool);
    let mut tool_indices = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == Role::Tool)
        .map(|(idx, message)| (idx, message.content.chars().count()))
        .collect::<Vec<_>>();
    tool_indices.sort_by_key(|(idx, len)| (Some(*idx) == newest_tool, std::cmp::Reverse(*len)));

    for (idx, _) in tool_indices {
        let total = context_chars(messages);
        if total <= target_chars {
            break;
        }
        let current = messages[idx].content.chars().count();
        let desired = current
            .saturating_sub(total.saturating_sub(target_chars))
            .max(256);
        messages[idx].content = truncate_context_text(&messages[idx].content, desired);
    }

    // Extremely large dialogue messages must not defeat the hard ceiling. Keep
    // the newest user message until last, and never mutate system instructions.
    let newest_user = messages
        .iter()
        .rposition(|message| message.role == Role::User);
    let mut dialogue_indices = messages
        .iter()
        .enumerate()
        .filter(|(idx, message)| {
            matches!(message.role, Role::User | Role::Assistant)
                && Some(*idx) != newest_user
                && !message.content.is_empty()
        })
        .map(|(idx, _)| idx)
        .collect::<Vec<_>>();
    if let Some(idx) = newest_user {
        dialogue_indices.push(idx);
    }
    for idx in dialogue_indices {
        let total = context_chars(messages);
        if total <= max_chars {
            break;
        }
        let current = messages[idx].content.chars().count();
        let desired = current
            .saturating_sub(total.saturating_sub(max_chars))
            .max(256);
        messages[idx].content = truncate_context_text(&messages[idx].content, desired);
    }

    tracing::info!(
        before_chars = before,
        after_chars = context_chars(messages),
        max_chars,
        target_chars,
        retained_messages = messages.len(),
        "Bounded prompt context"
    );
}

fn context_chars(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| message.content.chars().count())
        .sum()
}

fn truncate_context_text(content: &str, max_chars: usize) -> String {
    const MARKER: &str = "\n[Context truncated]";
    let content_chars = content.chars().count();
    if content_chars <= max_chars {
        return content.to_string();
    }
    let marker_chars = MARKER.chars().count();
    if max_chars <= marker_chars {
        return MARKER.chars().take(max_chars).collect();
    }
    let mut truncated = content
        .chars()
        .take(max_chars - marker_chars)
        .collect::<String>();
    truncated.push_str(MARKER);
    truncated
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

        if message.role == Role::Assistant
            && let Some(tool_calls_value) = &message.tool_calls
        {
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

        sanitized.push(message.clone());
        idx += 1;
    }

    if sanitized.len() != messages.len() {
        *messages = sanitized;
    }
}
