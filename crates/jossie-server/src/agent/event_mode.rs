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

    let result = AssertUnwindSafe(generate_event_message_inner(state, conversation_id, event))
        .catch_unwind()
        .await;

    {
        let mut active = state.active_conversations.write().await;
        active.remove(&conversation_id);
    }

    match result {
        Ok(result) => result,
        Err(payload) => {
            let panic_message = panic_payload_to_string(payload);
            tracing::error!(
                "Event-mode agent loop panicked for conversation {conversation_id}: {panic_message}"
            );
            anyhow::bail!("Event-mode agent loop panicked: {panic_message}")
        }
    }
}

#[derive(Debug, Deserialize)]
struct EventModeResponse {
    action: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    what_happened: String,
    #[serde(default)]
    why_now: String,
    #[serde(default)]
    what_changed: String,
    #[serde(default)]
    suggested_action: String,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    interrupt_score: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EventNotificationState {
    fingerprint: String,
    sent_at: String,
}

fn event_mode_output_format() -> jossie_llm::StructuredOutputFormat {
    jossie_llm::StructuredOutputFormat {
        name: "event_notification_decision".to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["notify", "skip"]},
                "message": {"type": "string"},
                "what_happened": {"type": "string"},
                "why_now": {"type": "string"},
                "what_changed": {"type": "string"},
                "suggested_action": {"type": "string"},
                "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                "interrupt_score": {"type": "number", "minimum": 0, "maximum": 1}
            },
            "required": [
                "action", "message", "what_happened", "why_now", "what_changed",
                "suggested_action", "confidence", "interrupt_score"
            ],
            "additionalProperties": false
        }),
    }
}

const EVENT_NOTIFICATION_MARKER: &str = "integration_event_notification";
const EVENT_MODE_SETTINGS_NAMESPACE: &str = "event_mode";
const EVENT_NOTIFY_COOLDOWN_SECONDS: i64 = 120;
const EVENT_MODE_MAX_ITERATIONS: usize = 3;
const EVENT_TOOL_READ_TRIGGER_EMAIL: &str = "event_read_trigger_email";
const EVENT_TOOL_READ_BATCH_EMAIL: &str = "event_read_batch_email";
const EVENT_NOTIFICATION_HISTORY_LIMIT: usize = 3;
const EVENT_NOTIFY_CONFIDENCE_THRESHOLD: f32 = 0.55;
const EVENT_NOTIFY_INTERRUPT_THRESHOLD: f32 = 0.65;
const HEARTBEAT_EVENT_TYPE: &str = "heartbeat_check";

async fn generate_event_message_inner(
    state: &AppState,
    conversation_id: Uuid,
    event: &IntegrationEvent,
) -> anyhow::Result<Option<String>> {
    let mut messages = state
        .db
        .get_messages(conversation_id, Some(state.event_max_context_messages))
        .await?;
    let recent_notification_context = build_recent_notification_context(&messages);
    messages.retain(|m| !is_event_notification_message(m));
    strip_tool_activity_from_event_context(&mut messages);

    sanitize_context_window(&mut messages);
    bound_context_window(
        &mut messages,
        (state.max_context_chars / 3).max(20_000),
        (state.context_compact_target_chars / 3).max(15_000),
        state.context_keep_recent_dialogue_messages.min(8),
    );

    let event_context = build_event_context_hint(event);
    let context_snapshot = snapshot_recent_dialogue(&messages, LIVE_STANCE_MESSAGE_WINDOW);
    let mut prompt = build_system_prompt(
        state,
        Some(conversation_id),
        Some(&event_context),
        Some(&context_snapshot),
        PromptMemoryScope::Event,
    )
    .await;
    let event_memory_context =
        build_event_specific_prompt_memory_context(state, event, &prompt.included_memory_keys)
            .await;
    if !event_memory_context.is_empty() {
        prompt.dynamic.push_str("\n\n");
        prompt.dynamic.push_str(&event_memory_context);
    }
    if !recent_notification_context.is_empty() {
        prompt.dynamic.push_str("\n\n");
        prompt.dynamic.push_str(&recent_notification_context);
    }
    if event.event_type == HEARTBEAT_EVENT_TYPE {
        prompt.dynamic.push_str("\n\n");
        prompt.dynamic.push_str(HEARTBEAT_MODE_ADDENDUM);
    }

    let prompt_cache_key = prompt.cache_key("event");
    prompt.insert_into(&mut messages);

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

    let tools = build_event_mode_tools(state, event);
    let event_output_format = event_mode_output_format();
    let fingerprint = event_notification_fingerprint(event);
    let mut previous_response_id: Option<String> = None;
    let mut chained_messages = Vec::new();

    for _ in 0..EVENT_MODE_MAX_ITERATIONS {
        let output = complete_agent_iteration(
            state,
            &messages,
            &chained_messages,
            &tools,
            previous_response_id.as_deref(),
            &prompt_cache_key,
            Some(&event_output_format),
        )
        .await?;
        previous_response_id = output.response_id.clone();
        chained_messages.clear();
        let content = output.content;
        let tool_calls = output.tool_calls;
        let response_items = output.response_items;

        if tool_calls.is_empty() {
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
                    if !decision.should_notify() {
                        tracing::debug!(
                            "Skipping event notification for conversation {} because confidence/interrupt score was too low: confidence={:?} interrupt_score={:?}",
                            conversation_id,
                            decision.confidence,
                            decision.interrupt_score
                        );
                        return Ok(None);
                    }
                    if should_suppress_event_notification(state, conversation_id, &fingerprint)
                        .await?
                    {
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
            return Ok(None);
        }

        let assistant_msg = Message::transient(Role::Assistant, content.clone())
            .with_tool_calls(serde_json::to_value(&tool_calls)?)
            .with_response_items(response_items);
        messages.push(assistant_msg);

        for call in tool_calls {
            let result = execute_event_mode_tool(state, event, &call).await;
            let tool_msg = match result {
                Ok(result) => Message::transient(Role::Tool, result.content)
                    .with_tool_call_id(call.id.clone())
                    .with_name(call.name.clone()),
                Err(err) => Message::transient(Role::Tool, format!("Error: {err}"))
                    .with_tool_call_id(call.id.clone())
                    .with_name(call.name.clone()),
            };
            messages.push(tool_msg.clone());
            chained_messages.push(tool_msg);
        }
    }

    tracing::warn!(
        "Event mode exceeded max iterations ({EVENT_MODE_MAX_ITERATIONS}) for conversation {conversation_id}"
    );
    Ok(None)
}

fn build_event_mode_tools(
    state: &AppState,
    event: &IntegrationEvent,
) -> Vec<jossie_core::ToolDefinition> {
    let event_capabilities: &[CapabilityGroup] = match event.event_type.as_str() {
        "new_email" | "gmail_new_message" | "new_email_batch" => &[CapabilityGroup::Mail],
        "calendar_event" | "calendar_event_updated" | "calendar_event_batch" => {
            &[CapabilityGroup::Calendar]
        }
        // Self-initiated check-ins get no pre-fetched context; grant a broader (still
        // read-only) set of tools so the model can look around before deciding, rather
        // than judging from the bare heartbeat payload alone.
        HEARTBEAT_EVENT_TYPE => &[
            CapabilityGroup::Memory,
            CapabilityGroup::Knowledge,
            CapabilityGroup::Mail,
            CapabilityGroup::Calendar,
            CapabilityGroup::Scheduler,
        ],
        _ => &[],
    };
    let mut tools: Vec<jossie_core::ToolDefinition> = state
        .registry
        .all_agent_tool_definitions()
        .into_iter()
        .filter(|tool| {
            let metadata = state.registry.metadata_for(&jossie_core::ToolCall {
                id: String::new(),
                name: tool.name.clone(),
                arguments: "{}".to_string(),
            });
            metadata.effect == jossie_core::integration::ToolEffect::Read
                && event_capabilities.contains(&metadata.capability)
        })
        .collect();
    if event_supports_trigger_email_read(event) {
        tools.push(jossie_core::ToolDefinition {
            name: EVENT_TOOL_READ_TRIGGER_EMAIL.to_string(),
            description: "Read the full triggering email before notifying when the summary alone is not enough.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }),
        });
    }
    if event.event_type == "new_email_batch"
        && event
            .payload
            .get("emails")
            .and_then(|v| v.as_array())
            .is_some_and(|emails| !emails.is_empty())
    {
        tools.push(jossie_core::ToolDefinition {
            name: EVENT_TOOL_READ_BATCH_EMAIL.to_string(),
            description: "Read one specific email from the current batch by 1-based index when you need more context before notifying.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "index": {
                        "type": "integer",
                        "description": "1-based index of the email in the batch payload"
                    }
                },
                "required": ["index"],
                "additionalProperties": false
            }),
        });
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools.dedup_by(|a, b| a.name == b.name);
    tools
}

fn event_supports_trigger_email_read(event: &IntegrationEvent) -> bool {
    match event.event_type.as_str() {
        "gmail_new_message" => event
            .payload
            .get("message_id")
            .and_then(|v| v.as_str())
            .is_some(),
        "new_email" => event.payload.get("uid").and_then(|v| v.as_u64()).is_some(),
        _ => false,
    }
}

async fn execute_event_mode_tool(
    state: &AppState,
    event: &IntegrationEvent,
    call: &jossie_core::ToolCall,
) -> anyhow::Result<jossie_core::ToolResult> {
    let delegated = match call.name.as_str() {
        EVENT_TOOL_READ_TRIGGER_EMAIL => build_trigger_email_read_call(event, &call.id)?,
        EVENT_TOOL_READ_BATCH_EMAIL => build_batch_email_read_call(event, call)?,
        _ => call.clone(),
    };
    let result = state.registry.execute(&delegated).await;
    Ok(jossie_core::ToolResult {
        tool_call_id: call.id.clone(),
        content: result.content,
        is_error: result.is_error,
    })
}

fn build_trigger_email_read_call(
    event: &IntegrationEvent,
    tool_call_id: &str,
) -> anyhow::Result<jossie_core::ToolCall> {
    match event.event_type.as_str() {
        "gmail_new_message" => {
            let message_id = event
                .payload
                .get("message_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("event is missing Gmail message_id"))?;
            let account_id = event
                .payload
                .get("account_id")
                .and_then(|v| v.as_str())
                .unwrap_or(event.account_id.as_str());
            Ok(jossie_core::ToolCall {
                id: tool_call_id.to_string(),
                name: "mail_read".to_string(),
                arguments: serde_json::json!({
                    "message_ref": {
                        "provider": "gmail",
                        "account_id": format!("gmail:{account_id}"),
                        "external_id": message_id,
                        "mailbox": serde_json::Value::Null,
                        "native": serde_json::Value::Null
                    }
                })
                .to_string(),
            })
        }
        "new_email" => {
            let uid = event
                .payload
                .get("uid")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("event is missing IMAP uid"))?;
            let folder = event
                .payload
                .get("folder")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let account_id = event
                .payload
                .get("account_id")
                .and_then(|v| v.as_str())
                .unwrap_or(event.account_id.as_str());
            Ok(jossie_core::ToolCall {
                id: tool_call_id.to_string(),
                name: "mail_read".to_string(),
                arguments: serde_json::json!({
                    "message_ref": {
                        "provider": "imap",
                        "account_id": format!("imap:{account_id}"),
                        "external_id": uid.to_string(),
                        "mailbox": (!folder.is_empty()).then_some(folder),
                        "native": { "uid": uid }
                    }
                })
                .to_string(),
            })
        }
        _ => anyhow::bail!("event does not support reading a trigger email"),
    }
}

fn build_batch_email_read_call(
    event: &IntegrationEvent,
    call: &jossie_core::ToolCall,
) -> anyhow::Result<jossie_core::ToolCall> {
    #[derive(Deserialize)]
    struct Args {
        index: usize,
    }

    if event.event_type != "new_email_batch" {
        anyhow::bail!("batch email reader only works for new_email_batch events");
    }

    let args: Args = serde_json::from_str(&call.arguments)?;
    anyhow::ensure!(args.index > 0, "batch email index must be 1 or greater");

    let emails = event
        .payload
        .get("emails")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("event batch is missing emails"))?;
    let Some(selected) = emails.get(args.index - 1) else {
        anyhow::bail!("batch email index {} is out of range", args.index);
    };

    let selected_event: IntegrationEvent = serde_json::from_value(selected.clone())?;
    build_trigger_email_read_call(&selected_event, &call.id)
}

fn is_event_notification_message(message: &Message) -> bool {
    message.role == Role::Assistant && message.name.as_deref() == Some(EVENT_NOTIFICATION_MARKER)
}

impl EventModeResponse {
    fn has_minimal_brief(&self) -> bool {
        !self.what_happened.trim().is_empty()
            && !self.why_now.trim().is_empty()
            && (!self.what_changed.trim().is_empty() || !self.suggested_action.trim().is_empty())
    }

    fn confidence_value(&self) -> f32 {
        self.confidence.unwrap_or(0.0).clamp(0.0, 1.0)
    }

    fn interrupt_score_value(&self) -> f32 {
        self.interrupt_score.unwrap_or(0.0).clamp(0.0, 1.0)
    }

    fn should_notify(&self) -> bool {
        self.has_minimal_brief()
            && self.confidence_value() >= EVENT_NOTIFY_CONFIDENCE_THRESHOLD
            && self.interrupt_score_value() >= EVENT_NOTIFY_INTERRUPT_THRESHOLD
    }
}

fn build_recent_notification_context(messages: &[Message]) -> String {
    let recent = messages
        .iter()
        .rev()
        .filter(|message| is_event_notification_message(message))
        .take(EVENT_NOTIFICATION_HISTORY_LIMIT)
        .map(|message| {
            format!(
                "- {} ago: {}",
                format_relative_age(message),
                preview_text(&message.content, 160)
            )
        })
        .collect::<Vec<_>>();

    if recent.is_empty() {
        return String::new();
    }

    let mut section = String::from(
        "## Recent Notification Delivery Context\nAvoid redundant interruptions if this event overlaps with what was just surfaced:\n",
    );
    for entry in recent.into_iter().rev() {
        section.push_str(&entry);
        section.push('\n');
    }
    section
}

fn format_relative_age(message: &Message) -> String {
    let age = chrono::Utc::now().signed_duration_since(message.created_at);
    if age.num_minutes() < 1 {
        "less than a minute".to_string()
    } else if age.num_hours() < 1 {
        format!("{} minute(s)", age.num_minutes())
    } else if age.num_days() < 1 {
        format!("{} hour(s)", age.num_hours())
    } else {
        format!("{} day(s)", age.num_days())
    }
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

async fn build_event_specific_prompt_memory_context(
    state: &AppState,
    event: &IntegrationEvent,
    excluded_keys: &HashSet<String>,
) -> String {
    let query = build_event_memory_query(event);
    if query.trim().is_empty() {
        return String::new();
    }

    let Ok(entries) = state
        .db
        .memory_prompt_search(
            PromptMemoryScope::Event.as_str(),
            &query,
            MAX_EVENT_PROMPT_MEMORY_MATCHES,
        )
        .await
    else {
        return String::new();
    };

    let entries = entries
        .into_iter()
        .filter(|entry| {
            !is_builtin_profile_memory(&entry.key) && !excluded_keys.contains(&entry.key)
        })
        .collect::<Vec<_>>();

    format_prompt_memory_section(
        "## Event-Specific Memory Matches\nThese prompt-eligible memories match this event's sender, subject, title, integration, or event type.\n",
        &entries,
        240,
    )
}

fn build_event_memory_query(event: &IntegrationEvent) -> String {
    let mut terms = vec![
        event.integration.as_str(),
        event.event_type.as_str(),
        event.account_id.as_str(),
    ]
    .into_iter()
    .filter(|term| !term.trim().is_empty())
    .map(str::to_string)
    .collect::<Vec<_>>();

    match event.event_type.as_str() {
        "gmail_new_message" | "new_email" => {
            push_payload_str(&event.payload, "from", &mut terms);
            push_payload_str(&event.payload, "sender", &mut terms);
            push_payload_str(&event.payload, "subject", &mut terms);
        }
        "new_email_batch" => {
            if let Some(emails) = event.payload.get("emails").and_then(|v| v.as_array()) {
                for email in emails.iter().take(8) {
                    if let Some(payload) = email.get("payload") {
                        push_payload_str(payload, "from", &mut terms);
                        push_payload_str(payload, "sender", &mut terms);
                        push_payload_str(payload, "subject", &mut terms);
                    }
                }
            }
        }
        "calendar_event" | "calendar_event_updated" => {
            push_payload_str(&event.payload, "summary", &mut terms);
            push_payload_str(&event.payload, "organizer", &mut terms);
            push_payload_str(&event.payload, "location", &mut terms);
            push_payload_str(&event.payload, "calendar_id", &mut terms);
        }
        "calendar_event_batch" => {
            if let Some(events) = event.payload.get("events").and_then(|v| v.as_array()) {
                for item in events.iter().take(8) {
                    if let Some(payload) = item.get("payload") {
                        push_payload_str(payload, "summary", &mut terms);
                        push_payload_str(payload, "organizer", &mut terms);
                        push_payload_str(payload, "location", &mut terms);
                        push_payload_str(payload, "calendar_id", &mut terms);
                    }
                }
            }
        }
        _ => {}
    }

    terms
        .join(" ")
        .split_whitespace()
        .take(80)
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_payload_str(payload: &serde_json::Value, key: &str, terms: &mut Vec<String>) {
    if let Some(value) = payload.get(key).and_then(|v| v.as_str()) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            terms.push(trimmed.to_string());
        }
    }
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

