    fn mk_event(
        id: &str,
        event_type: &str,
        integration: &str,
        account_id: &str,
        payload: serde_json::Value,
    ) -> IntegrationEvent {
        IntegrationEvent {
            id: id.to_string(),
            integration: integration.to_string(),
            account_id: account_id.to_string(),
            event_type: event_type.to_string(),
            dedupe_key: format!("dedupe_{id}"),
            payload,
            status: "new".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            processed_at: None,
            last_error: None,
        }
    }

    #[test]
    fn identifies_email_event_types() {
        let email_event = mk_event("1", "new_email", "email", "acc_1", serde_json::json!({}));
        let gmail_event = mk_event(
            "2",
            "gmail_new_message",
            "google",
            "acc_2",
            serde_json::json!({}),
        );
        let calendar_event = mk_event(
            "3",
            "calendar_event",
            "google",
            "acc_2",
            serde_json::json!({}),
        );

        assert!(is_email_event(&email_event));
        assert!(is_email_event(&gmail_event));
        assert!(!is_email_event(&calendar_event));
    }

    #[test]
    fn identifies_calendar_event_types() {
        let calendar_event = mk_event(
            "1",
            "calendar_event_updated",
            "google",
            "acc_1",
            serde_json::json!({}),
        );
        let email_event = mk_event(
            "2",
            "gmail_new_message",
            "google",
            "acc_1",
            serde_json::json!({}),
        );

        assert!(is_calendar_event(&calendar_event));
        assert!(!is_calendar_event(&email_event));
    }

    #[test]
    fn builds_single_batched_email_event() {
        let e1 = mk_event(
            "1",
            "gmail_new_message",
            "google",
            "acc_1",
            serde_json::json!({
                "from": "alice@example.com",
                "subject": "Subject 1"
            }),
        );
        let e2 = mk_event(
            "2",
            "gmail_new_message",
            "google",
            "acc_1",
            serde_json::json!({
                "from": "bob@example.com",
                "subject": "Subject 2"
            }),
        );

        let batch = build_email_batch_event(&[e1, e2]);
        assert_eq!(batch.event_type, "new_email_batch");
        assert_eq!(batch.integration, "google");
        assert_eq!(batch.account_id, "acc_1");
        assert_eq!(batch.payload["count"], serde_json::json!(2));
        assert_eq!(batch.payload["emails"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn batches_mixed_sources_as_mixed() {
        let e1 = mk_event("1", "new_email", "email", "acc_1", serde_json::json!({}));
        let e2 = mk_event(
            "2",
            "gmail_new_message",
            "google",
            "acc_2",
            serde_json::json!({}),
        );

        let batch = build_email_batch_event(&[e1, e2]);
        assert_eq!(batch.integration, "mixed");
        assert_eq!(batch.account_id, "mixed");
    }

    #[test]
    fn reduces_calendar_events_with_dedupe_and_filtering() {
        let old = mk_event(
            "old",
            "calendar_event_updated",
            "google",
            "acc_1",
            serde_json::json!({
                "calendar_id": "primary",
                "summary": "Standup",
                "status": "confirmed",
                "start": "2026-02-10T10:00:00Z",
                "end": "2026-02-10T10:15:00Z",
                "updated": "2026-02-09T10:00:00Z"
            }),
        );
        let new = mk_event(
            "new",
            "calendar_event_updated",
            "google",
            "acc_1",
            serde_json::json!({
                "calendar_id": "primary",
                "summary": "Standup",
                "status": "confirmed",
                "start": "2026-02-10T10:00:00Z",
                "end": "2026-02-10T10:15:00Z",
                "updated": "2026-02-09T11:00:00Z"
            }),
        );
        let low_value = mk_event(
            "noise",
            "calendar_event_updated",
            "google",
            "acc_1",
            serde_json::json!({
                "summary": "Untitled",
                "status": "cancelled",
                "start": "2000-01-01T00:00:00Z",
                "updated": "2026-02-09T12:00:00Z"
            }),
        );

        let (reduced, omitted) = reduce_calendar_events(&[old, new.clone(), low_value], 50);
        assert_eq!(reduced.len(), 1);
        assert_eq!(omitted, 2);
        assert_eq!(reduced[0].id, new.id);
    }

    #[test]
    fn cron_occurrence_advances_to_next_matching_minute() {
        let after = DateTime::parse_from_rfc3339("2026-02-10T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // Every day at 08:30.
        let next = next_cron_occurrence("30 8 * * *", after).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-02-10T08:30:00+00:00");
    }

    #[test]
    fn cron_occurrence_respects_day_of_week() {
        // 2026-02-10 is a Tuesday; "weekday mornings" should skip to the next
        // matching day once today's slot has already passed.
        let after = DateTime::parse_from_rfc3339("2026-02-10T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = next_cron_occurrence("0 8 * * 1-5", after).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-02-11T08:00:00+00:00");
    }

    #[test]
    fn cron_occurrence_rejects_invalid_expression() {
        assert!(next_cron_occurrence("not a cron expression", Utc::now()).is_err());
    }

    #[test]
    fn heartbeat_is_due_when_never_run() {
        assert!(heartbeat_is_due(None, 3600, Utc::now()));
    }

    #[test]
    fn heartbeat_is_due_when_unparseable_timestamp() {
        assert!(heartbeat_is_due(Some("not-a-timestamp"), 3600, Utc::now()));
    }

    #[test]
    fn heartbeat_is_not_due_before_interval_elapses() {
        let now = Utc::now();
        let last_run = (now - chrono::Duration::seconds(1800)).to_rfc3339();
        assert!(!heartbeat_is_due(Some(&last_run), 3600, now));
    }

    #[test]
    fn heartbeat_is_due_once_interval_elapses() {
        let now = Utc::now();
        let last_run = (now - chrono::Duration::seconds(3601)).to_rfc3339();
        assert!(heartbeat_is_due(Some(&last_run), 3600, now));
    }

    #[test]
    fn due_within_window_accepts_near_future() {
        let now = Utc::now();
        let next_run = (now + chrono::Duration::hours(2)).to_rfc3339();
        assert!(is_due_within_window(Some(&next_run), 24, now));
    }

    #[test]
    fn due_within_window_rejects_past() {
        let now = Utc::now();
        let next_run = (now - chrono::Duration::minutes(5)).to_rfc3339();
        assert!(!is_due_within_window(Some(&next_run), 24, now));
    }

    #[test]
    fn due_within_window_rejects_beyond_window() {
        let now = Utc::now();
        let next_run = (now + chrono::Duration::hours(48)).to_rfc3339();
        assert!(!is_due_within_window(Some(&next_run), 24, now));
    }

    #[test]
    fn due_within_window_rejects_missing_next_run() {
        assert!(!is_due_within_window(None, 24, Utc::now()));
    }
