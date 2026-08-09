#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_structured_imap_search_query() {
        let query = build_imap_search_query(
            None,
            &["receipt".to_string(), "invoice".to_string()],
            "any",
            Some("shop@example.com"),
            None,
            Some("2026-07-01"),
            Some("2026-08-01"),
        )
        .unwrap();
        assert_eq!(
            query,
            "SINCE 01-Jul-2026 BEFORE 01-Aug-2026 FROM \"shop@example.com\" OR TEXT \"receipt\" TEXT \"invoice\""
        );
    }

    #[test]
    fn rejects_invalid_imap_filter_date() {
        assert!(
            build_imap_search_query(None, &[], "any", None, None, Some("07/01/2026"), None)
                .is_err()
        );
    }

    #[test]
    fn extract_message_body_prefers_plaintext() {
        let raw = concat!(
            "Subject: Test\r\n",
            "From: sender@example.com\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/alternative; boundary=\"ALT\"\r\n",
            "\r\n",
            "--ALT\r\n",
            "Content-Type: text/plain; charset=UTF-8\r\n",
            "\r\n",
            "Hello from plain text.\r\n",
            "--ALT\r\n",
            "Content-Type: text/html; charset=UTF-8\r\n",
            "\r\n",
            "<html><body><p>Hello from <b>HTML</b>.</p></body></html>\r\n",
            "--ALT--\r\n"
        );

        let parsed = mailparse::parse_mail(raw.as_bytes()).expect("mail should parse");
        let body = extract_message_body(&parsed);
        assert!(body.contains("Hello from plain text."));
    }

    #[test]
    fn extract_message_body_falls_back_to_html() {
        let raw = concat!(
            "Subject: HTML only\r\n",
            "From: sender@example.com\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: text/html; charset=UTF-8\r\n",
            "\r\n",
            "<html><body><h1>Meeting</h1><p>Tomorrow at 10:00.</p></body></html>\r\n"
        );

        let parsed = mailparse::parse_mail(raw.as_bytes()).expect("mail should parse");
        let body = extract_message_body(&parsed);
        assert!(body.contains("Meeting"));
        assert!(body.contains("Tomorrow at 10:00."));
    }

    #[test]
    fn extract_header_fields_reads_common_headers() {
        let raw = concat!(
            "From: sender@example.com\r\n",
            "Subject: Subject line\r\n",
            "Date: Tue, 10 Feb 2026 09:00:00 +0000\r\n",
            "\r\n"
        );

        let (from, subject, date) = extract_header_fields(raw.as_bytes());
        assert_eq!(from, "sender@example.com");
        assert_eq!(subject, "Subject line");
        assert_eq!(date, "Tue, 10 Feb 2026 09:00:00 +0000");
    }

    #[test]
    fn mailbox_poll_seeds_cursor_for_first_sync() {
        let action = EmailIntegration::plan_mailbox_poll(None, None, Some(42), Some(7));
        assert_eq!(action, MailboxPollAction::SeedCursor { last_seen_uid: 41 });
    }

    #[test]
    fn mailbox_poll_reseeds_when_uid_validity_changes() {
        let action = EmailIntegration::plan_mailbox_poll(Some(10), Some(7), Some(15), Some(8));
        assert_eq!(action, MailboxPollAction::SeedCursor { last_seen_uid: 14 });
    }

    #[test]
    fn mailbox_poll_detects_no_change_from_uid_next() {
        let action = EmailIntegration::plan_mailbox_poll(Some(10), Some(7), Some(11), Some(7));
        assert_eq!(action, MailboxPollAction::NoChange);
    }

    #[test]
    fn mailbox_poll_fetches_from_next_uid() {
        let action = EmailIntegration::plan_mailbox_poll(Some(10), Some(7), Some(14), Some(7));
        assert_eq!(action, MailboxPollAction::PollFrom { start_uid: 11 });
    }

    #[test]
    fn parse_header_summary_extracts_message_id_and_recipients() {
        let raw = concat!(
            "Message-ID: <abc@example.com>\r\n",
            "From: sender@example.com\r\n",
            "To: one@example.com, Two Person <two@example.com>\r\n",
            "Subject: Subject line\r\n",
            "Date: Tue, 10 Feb 2026 09:00:00 +0000\r\n",
            "\r\n"
        );

        let summary = parse_header_summary(raw.as_bytes());
        assert_eq!(summary.message_id.as_deref(), Some("<abc@example.com>"));
        assert_eq!(summary.from, "sender@example.com");
        assert_eq!(
            summary.to,
            vec![
                "one@example.com".to_string(),
                "Two Person <two@example.com>".to_string()
            ]
        );
        assert_eq!(summary.subject, "Subject line");
    }

    #[test]
    fn build_message_unique_id_prefers_uid_validity() {
        assert_eq!(
            EmailIntegration::build_message_unique_id(Some(7), 42),
            "imap:7:42"
        );
        assert_eq!(
            EmailIntegration::build_message_unique_id(None, 42),
            "imap:42"
        );
    }

    #[test]
    fn provider_integration_exposes_no_legacy_agent_tools() {
        let integration = EmailIntegration::new(&EmailConfig::default());
        assert!(integration.tools().is_empty());
    }
}
