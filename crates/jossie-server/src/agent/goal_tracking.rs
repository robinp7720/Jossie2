// --- Goal Tracking (#4) ---

const MAX_TRACKED_STEPS: usize = 8;
const MAX_TRACKED_OBSERVATIONS: usize = 5;
const MAX_CHECKPOINT_RECORDS: usize = 12;
const MAX_RELEVANT_MEMORIES: usize = 4;
const MAX_PROMPT_MEMORIES: usize = 6;
const MAX_EVENT_PROMPT_MEMORY_MATCHES: usize = 4;
const LOOP_GUARD_WARN_THRESHOLD: usize = 2;
const LOOP_GUARD_STOP_THRESHOLD: usize = 3;
const PREMATURE_GOAL_FINAL_LIMIT: usize = 3;
const CONVERSATION_BUSY_RETRY_ATTEMPTS: usize = 30;
const CONVERSATION_BUSY_RETRY_DELAY_MS: u64 = 500;
const LIVE_STANCE_MESSAGE_WINDOW: usize = 6;
const REFLECTION_CONTEXT_WINDOW: usize = 4;

const INCOMING_NOTIFICATION_MODE_PROMPT: &str = "## Incoming Notification Mode\nThis is still Jossie: same judgment, same continuity, same general tool access as a normal conversation.\nThe difference is that you are deciding whether this newly arrived event deserves an interruption right now.\nDefault to quiet triage.\nNotify only if the event is urgent, time-sensitive, actionable, clearly relevant to the user, or materially changes their plans.\nSkip low-signal items such as newsletters, receipts, marketing mail, routine confirmations, automated churn, or minor non-actionable calendar edits.\nFor email batches, notify only when the batch as a whole suggests something worth surfacing now.\nInterpret this event independently as a fresh arrival.\nDo NOT imply that you made a prior mistake, correction, or retraction unless the event payload explicitly says so.\nFor `gmail_new_message` and `new_email_batch`, frame updates as newly arrived emails, even when similar to prior ones.\nUse tools normally when they materially improve confidence, especially before notifying about details hidden behind an email summary, snippet, attachment, or linked system.\nDo not claim room changes, schedule changes, requirement changes, or downstream consequences unless the email body or another checked source explicitly confirms them.\nIf an email mentions another system such as Moodle, an attachment, or a linked page that you did not verify, say only that the email mentions it.\nIf you notify, write it like Jossie: short, concrete, natural, and grounded in what you actually checked.\nBefore deciding, build a compact internal notification brief and use it to choose `notify` or `skip`.\nOnly notify when confidence and interrupt_score are both strong enough.\nReturn strict JSON only, with no markdown, in this exact shape:\n{\"action\":\"notify|skip\",\"message\":\"<short user-facing message>\",\"what_happened\":\"...\",\"why_now\":\"...\",\"what_changed\":\"...\",\"suggested_action\":\"...\",\"confidence\":0.0,\"interrupt_score\":0.0}";

const HEARTBEAT_MODE_ADDENDUM: &str = "## Heartbeat Check\nThis particular pass was not triggered by anything arriving. Nothing necessarily happened. You are checking in on your own initiative because enough time has passed since the last check.\nTreat `skip` as the strong default outcome; most heartbeats should produce nothing.\nOnly notify if you have a genuinely good, concrete reason to reach out right now: something time-sensitive is coming up and has not been mentioned, something you said you would follow up on is now due, or a clear gap in continuity would otherwise go unnoticed.\nDo not manufacture a reason to speak. Restating known facts, a general check-in, or \"just wanted to say hi\" are not reasons to notify.\nYou may use the available read tools to look at memory, the knowledge graph, upcoming scheduled items, or calendar/email if that materially changes your judgment, but do not go looking for a problem to report if nothing prompted this pass.\nHold this to a stricter bar than a real inbound event.";

#[derive(Clone, Copy)]
enum PromptMemoryScope {
    Chat,
    Event,
}

impl PromptMemoryScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Event => "event",
        }
    }
}

struct GoalTracker {
    primary_goal: String,
    durable_goal: Option<jossie_db::GoalWithTasks>,
    locked_goal_id: Option<String>,
    completed_steps: Vec<String>,
    observations: Vec<String>,
    last_tool_batch_signature: Option<String>,
    repeated_tool_batch_count: usize,
    successful_reads: HashMap<String, String>,
    checkpoint_records: Vec<serde_json::Value>,
    goal_bound_to_run: bool,
    scheduled_execution: bool,
}

impl GoalTracker {
    fn new(user_message: &str) -> Self {
        // Extract a concise goal from the user message (first 200 chars)
        let mut chars = user_message.chars();
        let prefix = chars.by_ref().take(200).collect::<String>();
        let goal = if chars.next().is_some() {
            format!("{prefix}...")
        } else {
            prefix
        };
        Self {
            primary_goal: goal,
            durable_goal: None,
            locked_goal_id: None,
            completed_steps: Vec::new(),
            observations: Vec::new(),
            last_tool_batch_signature: None,
            repeated_tool_batch_count: 0,
            successful_reads: HashMap::new(),
            checkpoint_records: Vec::new(),
            goal_bound_to_run: false,
            scheduled_execution: false,
        }
    }

    fn record_tool_calls(&mut self, calls: &[jossie_core::ToolCall]) {
        for call in calls {
            push_recent(
                &mut self.completed_steps,
                format!("{} {}", call.name, preview_text(&call.arguments, 100)),
                MAX_TRACKED_STEPS,
            );
        }
    }

    fn record_tool_result(
        &mut self,
        call: &jossie_core::ToolCall,
        result: &jossie_core::ToolResult,
    ) {
        let status = if result.is_error { "error" } else { "ok" };
        push_recent(
            &mut self.observations,
            format!(
                "{} ({status}): {}",
                call.name,
                preview_text(&result.content, 140)
            ),
            MAX_TRACKED_OBSERVATIONS,
        );
        if !result.is_error && tool_metadata(&call.name, &call.arguments).effect == ToolEffect::Read
        {
            self.successful_reads.insert(
                tool_call_signature(call),
                preview_text(&result.content, 400),
            );
        }
        self.checkpoint_records.push(serde_json::json!({
            "tool": call.name,
            "arguments": preview_text(&call.arguments, 400),
            "status": status,
            "result": preview_text(&result.content, 600),
        }));
        if self.checkpoint_records.len() > MAX_CHECKPOINT_RECORDS {
            self.checkpoint_records.remove(0);
        }
    }

    fn split_repeated_reads(
        &self,
        calls: Vec<jossie_core::ToolCall>,
    ) -> (
        Vec<jossie_core::ToolCall>,
        Vec<(usize, jossie_core::ToolCall, jossie_core::ToolResult)>,
    ) {
        let mut fresh = Vec::new();
        let mut repeated = Vec::new();
        for (idx, call) in calls.into_iter().enumerate() {
            let signature = tool_call_signature(&call);
            if tool_metadata(&call.name, &call.arguments).effect == ToolEffect::Read
                && let Some(previous) = self.successful_reads.get(&signature)
            {
                repeated.push((
                    idx,
                    call.clone(),
                    jossie_core::ToolResult {
                        tool_call_id: call.id.clone(),
                        content: format!(
                            "This identical read already succeeded in the current run. Reuse its result instead of repeating it. Previous result preview: {previous}"
                        ),
                        is_error: false,
                    },
                ));
            } else {
                fresh.push(call);
            }
        }
        (fresh, repeated)
    }

    fn note_tool_batch(&mut self, calls: &[jossie_core::ToolCall]) -> Option<String> {
        let signature = calls
            .iter()
            .map(|call| format!("{}:{}", call.name, preview_text(&call.arguments, 220)))
            .collect::<Vec<_>>()
            .join(" | ");

        if self.last_tool_batch_signature.as_deref() == Some(signature.as_str()) {
            self.repeated_tool_batch_count += 1;
        } else {
            self.last_tool_batch_signature = Some(signature);
            self.repeated_tool_batch_count = 1;
        }

        if self.repeated_tool_batch_count >= LOOP_GUARD_WARN_THRESHOLD {
            return Some(format!(
                "You have proposed the same tool batch {} times in a row. Do not repeat it unchanged. Either refine the inputs, explain the blocker clearly, or ask one focused question.",
                self.repeated_tool_batch_count
            ));
        }

        None
    }

    fn should_stop_for_repetition(&self) -> bool {
        self.repeated_tool_batch_count >= LOOP_GUARD_STOP_THRESHOLD
    }

    fn build_stuck_message(&self) -> String {
        let mut msg = String::from(
            "I’m not making useful progress because I keep reaching the same dead end.\n\n",
        );
        msg.push_str("What I’ve already checked:\n");
        if self.completed_steps.is_empty() {
            msg.push_str("- I haven’t completed a useful verification step yet.\n");
        } else {
            for step in &self.completed_steps {
                msg.push_str("- ");
                msg.push_str(step);
                msg.push('\n');
            }
        }
        if !self.observations.is_empty() {
            msg.push_str("\nWhat those checks showed:\n");
            for observation in &self.observations {
                msg.push_str("- ");
                msg.push_str(observation);
                msg.push('\n');
            }
        }
        msg.push_str(
            "\nI’m stopping here instead of looping. If you want, I can try a different angle or you can give me one extra constraint to narrow it down.",
        );
        msg
    }

    fn build_tracking_message(&self) -> String {
        let mut msg = format!("## Task State\nObjective: {}\n", self.primary_goal);
        if let Some(goal) = &self.durable_goal {
            msg.push_str(&format!(
                "Tracked goal: id={} {} ({}/{}) [{}]\n",
                goal.goal.id,
                goal.goal.title,
                goal.completed_tasks,
                goal.total_tasks,
                goal.goal.status
            ));
            for task in &goal.tasks {
                msg.push_str(&format!(
                    "- id={} [{}] {}",
                    task.id, task.status, task.title
                ));
                if let Some(blocker) = &task.blocker {
                    msg.push_str(&format!(" — blocked: {blocker}"));
                }
                msg.push('\n');
            }
            msg.push_str("Update this plan with update_work_plan when an outcome starts, completes, fails, or becomes blocked. Tool execution alone is not outcome completion.\n");
            if self.locked_goal_id.is_some() {
                msg.push_str("This run is locked to the tracked goal above. Update that goal only; never create a replacement goal.\n");
            }
        }
        if !self.completed_steps.is_empty() {
            msg.push_str("Recent completed checks:\n");
            for step in &self.completed_steps {
                msg.push_str("- ");
                msg.push_str(step);
                msg.push('\n');
            }
        }
        if !self.observations.is_empty() {
            msg.push_str("Recent observations:\n");
            for observation in &self.observations {
                msg.push_str("- ");
                msg.push_str(observation);
                msg.push('\n');
            }
        }
        if self.repeated_tool_batch_count >= LOOP_GUARD_WARN_THRESHOLD {
            msg.push_str("Loop warning:\n");
            msg.push_str("- Do not repeat the same tool call or search with unchanged inputs.\n");
        }
        msg.push_str("Next step rule:\n");
        msg.push_str(
            "- Either advance the task, explain the blocker, or ask one focused question.\n",
        );
        msg
    }

    fn active_goal_continuation_message(&self) -> Option<String> {
        let goal = self.durable_goal.as_ref()?;
        if !self.goal_bound_to_run || self.scheduled_execution || goal.goal.status != "active" {
            return None;
        }

        let unfinished = goal
            .tasks
            .iter()
            .filter(|task| !matches!(task.status.as_str(), "completed" | "cancelled"))
            .map(|task| format!("- [{}] {}", task.status, task.title))
            .collect::<Vec<_>>();
        let task_state = if unfinished.is_empty() {
            "All tasks look terminal, but the goal itself is still marked active.".to_string()
        } else {
            format!("Unfinished outcomes:\n{}", unfinished.join("\n"))
        };

        Some(format!(
            "[ACTIVE GOAL CONTINUATION]\nYour draft reply was not sent because it would leave a goal from this run active with no worker continuing it. Do not stop at a progress report. Continue the work now and use tools to advance the next unfinished outcome. Before giving the user a final reply, call update_work_plan by itself and mark the goal completed, or mark it blocked with the exact missing input.\nTracked goal: {}\n{}",
            goal.goal.title, task_state
        ))
    }
}

