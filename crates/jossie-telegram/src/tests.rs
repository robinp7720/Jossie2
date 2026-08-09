    fn sample_goal(status: &str, completed: bool) -> jossie_db::GoalWithTasks {
        let task_status = if completed { "completed" } else { "blocked" };
        jossie_db::GoalWithTasks {
            goal: jossie_db::Goal {
                id: "goal-1".to_string(),
                conversation_id: None,
                title: "Finish the expense review".to_string(),
                objective: "List every expense".to_string(),
                status: status.to_string(),
                blocker: (status == "blocked").then(|| "the July bank export".to_string()),
                archived_at: None,
                created_at: "now".to_string(),
                updated_at: "now".to_string(),
            },
            tasks: vec![jossie_db::GoalTask {
                id: "task-1".to_string(),
                goal_id: "goal-1".to_string(),
                position: 0,
                title: "Match the remaining transactions".to_string(),
                status: task_status.to_string(),
                summary: None,
                blocker: (!completed).then(|| "the July bank export".to_string()),
                source_type: None,
                source_id: None,
                created_at: "now".to_string(),
                updated_at: "now".to_string(),
            }],
            completed_tasks: usize::from(completed),
            total_tasks: 1,
        }
    }

    #[test]
    fn identifies_competing_get_updates_as_a_terminal_polling_conflict() {
        assert!(is_polling_conflict(&RequestError::Api(
            ApiError::TerminatedByOtherGetUpdates
        )));
        assert!(!is_polling_conflict(&RequestError::Api(
            ApiError::InvalidToken
        )));
    }

    #[test]
    fn split_message_uses_character_limit_and_word_boundaries() {
        let text = format!("{} {}", "😀".repeat(4090), "hello world");
        let chunks = split_message(&text, TELEGRAM_MESSAGE_LIMIT);
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= TELEGRAM_MESSAGE_LIMIT)
        );
        assert_eq!(chunks.join(" "), text);
    }

    #[test]
    fn supported_documents_reject_archives_and_executables() {
        assert!(supported_document("report.pdf", "application/pdf"));
        assert!(supported_document("notes.md", "text/markdown"));
        assert!(!supported_document("archive.zip", "application/zip"));
        assert!(!supported_document(
            "program.exe",
            "application/octet-stream"
        ));
    }

    #[test]
    fn callback_data_stays_within_telegram_limit() {
        let id = Uuid::new_v4().to_string();
        assert!(format!("pa:y:{id}").len() <= 64);
        assert!(format!("pa:n:{id}").len() <= 64);
    }

    #[test]
    fn goal_status_reads_like_conversation_not_internal_state() {
        let goal = sample_goal("blocked", false);
        let status = conversational_goal_status(Some(&goal));
        assert!(status.contains("I've kept our place"));
        assert!(status.contains("the July bank export"));
        assert!(status.contains("Once you send that"));
        assert!(!status.contains("goal_id"));
        assert!(!status.contains("blocked:"));
    }

    #[test]
    fn status_can_summarize_more_than_one_ongoing_goal() {
        let blocked = sample_goal("blocked", false);
        let mut active = sample_goal("active", false);
        active.goal.id = "goal-2".to_string();
        active.goal.title = "Plan the trip".to_string();
        let status = conversational_goals_status(&[blocked, active]);
        assert!(status.contains("2 things"));
        assert!(status.contains("Finish the expense review"));
        assert!(status.contains("Plan the trip"));
    }

    #[test]
    fn meaningful_goal_changes_are_added_to_the_chat_reply_once() {
        let before = sample_goal("active", false);
        let after = sample_goal("blocked", false);
        let reply = with_conversational_goal_update(
            "I found most of the transactions.".to_string(),
            Some(&before),
            Some(&after),
        );
        assert!(reply.starts_with("I found most of the transactions."));
        assert!(reply.contains("I've kept our place"));

        let unchanged = with_conversational_goal_update(
            "Still checking.".to_string(),
            Some(&after),
            Some(&after),
        );
        assert_eq!(unchanged, "Still checking.");
    }

    #[test]
    fn natural_continuation_and_requested_files_resume_work() {
        let goal = sample_goal("blocked", false);
        assert!(should_continue_tracked_goal(&goal, "continue", false));
        assert!(should_continue_tracked_goal(
            &goal,
            "Please continue!",
            false
        ));
        assert!(should_continue_tracked_goal(
            &goal,
            "here is the bank export",
            false
        ));
        assert!(should_continue_tracked_goal(
            &goal,
            "here is the export",
            true
        ));
        assert!(!should_continue_tracked_goal(
            &goal,
            "what is the weather?",
            false
        ));
    }

    #[test]
    fn blocked_work_produces_a_proactive_conversational_update() {
        let mut goal = sample_goal("blocked", false);
        goal.goal.blocker = Some(
            "Exact EUR card settlement amounts are missing; send the bank export.".to_string(),
        );
        assert!(goal_needs_proactive_notification(&goal, false));
        let message = proactive_goal_notification(&goal);
        assert!(message.contains("A quick update"));
        assert!(message.contains("Exact EUR card settlement amounts"));
        assert!(message.contains("Send that here"));
        assert!(!message.contains("status=blocked"));
    }

    #[test]
    fn notification_fingerprint_changes_with_the_blocker() {
        let first = sample_goal("blocked", false);
        let mut second = first.clone();
        second.goal.blocker = Some("a different document".to_string());
        assert_ne!(
            goal_notification_fingerprint(&first),
            goal_notification_fingerprint(&second)
        );
        let completed = sample_goal("completed", true);
        assert!(!goal_needs_proactive_notification(&completed, false));
        assert!(goal_needs_proactive_notification(&completed, true));
        let mut cancelled = sample_goal("cancelled", false);
        cancelled.tasks[0].status = "blocked".to_string();
        assert!(!goal_needs_proactive_notification(&cancelled, true));
    }

    #[tokio::test]
    async fn typing_status_is_sent_immediately_and_refreshed_until_stopped() {
        use axum::{Json, Router, extract::State};
        use serde_json::json;

        async fn record_action(State(calls): State<Arc<AtomicUsize>>) -> Json<serde_json::Value> {
            calls.fetch_add(1, Ordering::SeqCst);
            Json(json!({"ok": true, "result": true}))
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .fallback(record_action)
            .with_state(calls.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let bot = Bot::new("TEST").set_api_url(format!("http://{address}/").parse().unwrap());
        let stop = spawn_typing_with_interval(bot, ChatId(42), Duration::from_millis(15));
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = stop.send(());
        tokio::time::sleep(Duration::from_millis(10)).await;
        let stopped_at = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(35)).await;

        assert!(stopped_at >= 2);
        assert_eq!(calls.load(Ordering::SeqCst), stopped_at);
    }
