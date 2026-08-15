fn tool_call_signature(call: &jossie_core::ToolCall) -> String {
    let arguments = serde_json::from_str::<serde_json::Value>(&call.arguments)
        .ok()
        .and_then(|value| serde_json::to_string(&value).ok())
        .unwrap_or_else(|| call.arguments.clone());
    format!("{}:{arguments}", call.name)
}

fn push_recent(items: &mut Vec<String>, value: String, max_len: usize) {
    items.push(value);
    if items.len() > max_len {
        let overflow = items.len() - max_len;
        items.drain(0..overflow);
    }
}

fn snapshot_recent_dialogue(messages: &[Message], max_messages: usize) -> Vec<Message> {
    let mut snapshot = messages
        .iter()
        .rev()
        .filter(|message| {
            matches!(message.role, Role::User | Role::Assistant)
                && message.tool_call_id.is_none()
                && !message.content.trim().is_empty()
        })
        .take(max_messages)
        .cloned()
        .collect::<Vec<_>>();
    snapshot.reverse();
    snapshot
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn build_live_stance_context(messages: &[Message]) -> String {
    let recent = snapshot_recent_dialogue(messages, LIVE_STANCE_MESSAGE_WINDOW);
    if recent.len() < 2 {
        return String::new();
    }

    let last_user = recent
        .iter()
        .rev()
        .find(|message| message.role == Role::User)
        .map(|message| message.content.trim())
        .unwrap_or_default();
    if last_user.is_empty() {
        return String::new();
    }

    let combined = recent
        .iter()
        .map(|message| message.content.to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    let last_user_lower = last_user.to_lowercase();

    let mode = if contains_any(
        &combined,
        &[
            "feel",
            "felt",
            "frustrat",
            "upset",
            "sad",
            "angry",
            "hurt",
            "overreact",
            "relationship",
            "lonely",
        ],
    ) {
        "emotionally engaged and personal"
    } else if contains_any(
        &combined,
        &[
            "api", "bug", "error", "compile", "stack", "test", "rust", "code", "query", "trace",
        ],
    ) {
        "technical and problem-solving"
    } else if contains_any(
        &combined,
        &[
            "should i",
            "which",
            "choose",
            "pick",
            "send",
            "reply",
            "what should",
        ],
    ) {
        "decision-focused"
    } else if contains_any(
        &combined,
        &["why", "pattern", "what do you think", "how do i"],
    ) {
        "reflective and analytical"
    } else if contains_any(
        &combined,
        &["urgent", "asap", "right now", "tonight", "immediately"],
    ) {
        "time-sensitive and action-oriented"
    } else {
        "practical and conversational"
    };

    let directness = if contains_any(
        &last_user_lower,
        &[
            "just give me",
            "just answer",
            "be direct",
            "brief",
            "short",
            "concise",
            "quick answer",
            "cut straight",
            "just do it",
            "don't explain",
        ],
    ) || contains_any(
        &last_user_lower,
        &["damn", "stupid", "ridiculous", "wtf", "fuck"],
    ) {
        "blunt and compact"
    } else if mode == "emotionally engaged and personal" {
        "gentle but still plain"
    } else {
        "normal and direct"
    };

    let warmth = if mode == "emotionally engaged and personal" {
        "close and earned"
    } else if mode == "technical and problem-solving" {
        "light, low-friction, and not chatty"
    } else {
        "present and natural"
    };

    let response_bias = if directness == "blunt and compact"
        || matches!(
            mode,
            "technical and problem-solving"
                | "decision-focused"
                | "time-sensitive and action-oriented"
        ) {
        "answer first, explain only if it materially helps"
    } else {
        "lead with the core point, then expand only if needed"
    };

    let mut style_cues = Vec::new();
    if contains_any(
        &combined,
        &[
            "be direct",
            "just give me the answer",
            "just answer",
            "don't explain",
            "brief",
            "short",
        ],
    ) {
        style_cues.push("The user wants low-friction directness; avoid padding.");
    }
    if contains_any(&combined, &["don't ask", "just do it", "go ahead"]) {
        style_cues.push("Do not ask ceremonial permission when the next step is obvious.");
    }
    if contains_any(
        &combined,
        &["too formal", "too wordy", "generic", "robotic"],
    ) {
        style_cues.push("Avoid drifting into polished but generic assistant phrasing.");
    }
    if contains_any(
        &combined,
        &[
            "feel",
            "hurt",
            "frustrat",
            "upset",
            "sad",
            "relationship",
            "overreact",
        ],
    ) {
        style_cues.push("Warmth should be earned by the moment, not stock empathy.");
    }

    let mut section = String::from(
        "## Live Conversational Stance\nEstimate this from the recent dialogue and preserve it unless the user clearly shifts tone or goals.\n",
    );
    section.push_str(&format!("- Current mode: {mode}\n"));
    section.push_str(&format!("- Directness: {directness}\n"));
    section.push_str(&format!("- Warmth: {warmth}\n"));
    section.push_str(&format!("- Response bias: {response_bias}\n"));
    section.push_str(&format!(
        "- Open thread: {}\n",
        preview_text(last_user, 180)
    ));
    if !style_cues.is_empty() {
        for cue in style_cues {
            section.push_str("- Active style cue: ");
            section.push_str(cue);
            section.push('\n');
        }
    }
    section.push_str(
        "- Guardrail: Do not reset into generic assistant voice, over-explain, or add unearned softness.\n",
    );
    section
}

fn build_reflection_context(messages: &[Message]) -> String {
    let recent = snapshot_recent_dialogue(messages, REFLECTION_CONTEXT_WINDOW);
    if recent.is_empty() {
        return "No recent dialogue context available.".to_string();
    }

    recent
        .into_iter()
        .map(|message| {
            let speaker = if message.role == Role::User {
                "User"
            } else {
                "Assistant"
            };
            format!("{speaker}: {}", preview_text(&message.content, 220))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone)]
pub struct AgentRunOptions {
    pub allow_schedule_management: bool,
    pub allow_oob_messages: bool,
    pub scheduled_execution: bool,
    pub authorization_context: Option<String>,
    pub goal_id: Option<String>,
    pub task_id: Option<String>,
    pub work_source_type: Option<String>,
    pub work_source_id: Option<String>,
    pub work_summary: Option<String>,
    pub resume_checkpoint_run_id: Option<String>,
}

impl Default for AgentRunOptions {
    fn default() -> Self {
        Self {
            allow_schedule_management: true,
            allow_oob_messages: true,
            scheduled_execution: false,
            authorization_context: None,
            goal_id: None,
            task_id: None,
            work_source_type: None,
            work_source_id: None,
            work_summary: None,
            resume_checkpoint_run_id: None,
        }
    }
}

struct PromptBundle {
    stable: String,
    dynamic: String,
    included_memory_keys: HashSet<String>,
}

impl PromptBundle {
    fn cache_key(&self, model_scope: &str) -> String {
        let digest = Sha256::digest(self.stable.as_bytes());
        let digest = format!("{digest:x}");
        // The Responses API limits prompt_cache_key to 64 characters. A
        // 192-bit prefix remains ample for cache bucketing while leaving room
        // for a readable scope prefix.
        format!("jossie:{model_scope}:{}", &digest[..48])
    }

    fn insert_into(self, messages: &mut Vec<Message>) {
        if !self.dynamic.is_empty() {
            messages.insert(0, Message::transient(Role::System, self.dynamic));
        }
        messages.insert(0, Message::transient(Role::System, self.stable));
    }
}

async fn build_system_prompt(
    state: &AppState,
    conversation_id: Option<Uuid>,
    user_message: Option<&str>,
    context_messages: Option<&[Message]>,
    prompt_memory_scope: PromptMemoryScope,
) -> PromptBundle {
    let stable = match prompt_memory_scope {
        PromptMemoryScope::Chat => state.agent.system_prompt.clone(),
        PromptMemoryScope::Event => format!(
            "{}\n\n{}",
            state.agent.system_prompt, INCOMING_NOTIFICATION_MODE_PROMPT
        ),
    };
    let mut dynamic = String::new();

    // Add user-local time context for scheduling and personal-assistant decisions.
    let now = chrono::Local::now();
    dynamic.push_str(&format!(
        "Current Local Date and Time: {}",
        now.format("%A, %B %d, %Y %H:%M:%S %:z")
    ));

    let profile_future = state.db.get_profile_memories();
    let important_future = load_important_prompt_memories(state, prompt_memory_scope);
    let relevant_future = async {
        if let Some(message) = user_message {
            load_relevant_memories(state, message).await
        } else {
            Vec::new()
        }
    };
    let files_future = async {
        if let Some(conv_id) = conversation_id {
            state
                .db
                .list_files_for_conversation(conv_id)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    };
    let memory_stats_future = state.db.memory_stats();
    let graph_future = async {
        if let Some(message) = user_message {
            build_graph_context(state, message).await
        } else {
            String::new()
        }
    };
    let (profiles, important_memories, relevant_memories, files, memory_stats, graph_context) = tokio::join!(
        profile_future,
        important_future,
        relevant_future,
        files_future,
        memory_stats_future,
        graph_future
    );

    let profiles = profiles.unwrap_or_default();
    for (key, heading) in [
        ("agent_profile.soul", "## Agent Core Identity (Soul)"),
        ("agent_profile", "## Agent Description (Jossie)"),
        ("agent_profile.mood", "## Current Mood"),
        ("user_profile", "## User Description"),
    ] {
        if let Some(entry) = profiles.get(key) {
            dynamic.push_str("\n\n");
            dynamic.push_str(heading);
            dynamic.push('\n');
            dynamic.push_str(&entry.content);
        }
    }

    let mut included_memory_keys = HashSet::new();
    let important_memories = important_memories
        .into_iter()
        .filter(|entry| included_memory_keys.insert(entry.key.clone()))
        .collect::<Vec<_>>();
    let important_memory_context = format_prompt_memory_section(
        match prompt_memory_scope {
            PromptMemoryScope::Chat => {
                "## Important Chat Memory\nUse these durable preferences and traits unless the current user instruction overrides them.\n"
            }
            PromptMemoryScope::Event => {
                "## Important Event Memory\nUse these durable notification preferences and user traits when deciding whether an event matters.\n"
            }
        },
        &important_memories,
        260,
    );
    if !important_memory_context.is_empty() {
        dynamic.push_str("\n\n");
        dynamic.push_str(&important_memory_context);
    }

    if let Some(messages) = context_messages {
        let live_stance = build_live_stance_context(messages);
        if !live_stance.is_empty() {
            dynamic.push_str("\n\n");
            dynamic.push_str(&live_stance);
        }
    }

    let relevant_memories = relevant_memories
        .into_iter()
        .filter(|entry| included_memory_keys.insert(entry.key.clone()))
        .take(MAX_RELEVANT_MEMORIES)
        .collect::<Vec<_>>();
    let relevant_memory_context = format_relevant_memory_section(&relevant_memories);
    if !relevant_memory_context.is_empty() {
        dynamic.push_str("\n\n");
        dynamic.push_str(&relevant_memory_context);
    }

    if !files.is_empty() {
        dynamic.push_str("\n\n## Attached Files\nSupported images and documents are included directly while they remain in the model attachment budget. Use `read_file` for UTF-8 text or `ingest_chat_export` for chat exports. Audio is represented by its transcript in the user message:\n");
        for file in files {
            let kind = if file
                .mime_type
                .as_deref()
                .is_some_and(|mime| mime.starts_with("audio/"))
            {
                "audio; see transcript"
            } else {
                "attachment"
            };
            dynamic.push_str(&format!("- `{}` (ID: {}; {})\n", file.name, file.id, kind));
        }
    }

    if let Ok(stats) = memory_stats {
        dynamic.push_str(&format!(
            "\n\n## Memory Availability\n{} saved memories are available. Search memory when the injected context is insufficient.",
            stats.total
        ));
    }

    if !graph_context.is_empty() {
        dynamic.push_str("\n\n");
        dynamic.push_str(&graph_context);
    }

    tracing::debug!(
        stable_chars = stable.len(),
        dynamic_chars = dynamic.len(),
        included_memories = included_memory_keys.len(),
        "System prompt bundle built"
    );
    PromptBundle {
        stable,
        dynamic,
        included_memory_keys,
    }
}

async fn load_important_prompt_memories(
    state: &AppState,
    scope: PromptMemoryScope,
) -> Vec<MemoryPromptEntry> {
    let Ok(entries) = state
        .db
        .memory_prompt_context(scope.as_str(), MAX_PROMPT_MEMORIES)
        .await
    else {
        return Vec::new();
    };

    entries
        .into_iter()
        .filter(|entry| !is_builtin_profile_memory(&entry.key))
        .collect()
}

fn format_prompt_memory_section(
    heading: &str,
    entries: &[MemoryPromptEntry],
    preview_chars: usize,
) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut section = String::from(heading);
    for entry in entries {
        section.push_str("- `");
        section.push_str(&entry.key);
        section.push('`');
        section.push_str(&format!(" (importance {})", entry.importance));
        section.push_str(": ");
        section.push_str(&preview_text(&entry.content, preview_chars));
        section.push('\n');
    }
    section
}

fn is_builtin_profile_memory(key: &str) -> bool {
    matches!(
        key,
        "agent_profile" | "agent_profile.soul" | "agent_profile.mood" | "user_profile"
    )
}

async fn load_relevant_memories(state: &AppState, user_message: &str) -> Vec<MemoryEntry> {
    if user_message.trim().len() < 6 {
        return Vec::new();
    }

    let Ok(entries) = state.db.memory_search(user_message).await else {
        return Vec::new();
    };

    entries
        .into_iter()
        .filter(|entry| !is_builtin_profile_memory(&entry.key))
        .take(MAX_RELEVANT_MEMORIES)
        .collect()
}

fn format_relevant_memory_section(entries: &[MemoryEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut section = String::from(
        "## Potentially Relevant Memory\nThese entries look relevant to the current turn:\n",
    );
    for entry in entries {
        section.push_str("- `");
        section.push_str(&entry.key);
        section.push_str("`: ");
        section.push_str(&preview_text(&entry.content, 220));
        section.push('\n');
    }

    section
}

pub async fn prepend_system_prompt(
    state: &AppState,
    conversation_id: Option<Uuid>,
    messages: &mut Vec<Message>,
    user_message: Option<&str>,
) -> String {
    let context_snapshot = snapshot_recent_dialogue(messages, LIVE_STANCE_MESSAGE_WINDOW);
    let content = build_system_prompt(
        state,
        conversation_id,
        user_message,
        Some(&context_snapshot),
        PromptMemoryScope::Chat,
    )
    .await;
    let cache_key = content.cache_key("chat");
    content.insert_into(messages);
    cache_key
}
