#[async_trait::async_trait]
impl Integration for GoogleIntegration {
    fn name(&self) -> &str {
        "google"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::for_args::<EmptyToolArgs>(
                "google_list_accounts",
                "List configured Google accounts",
            ),
            ToolDefinition::for_args::<DriveSearchArgs>(
                "drive_search",
                "Search Google Drive files by name",
            ),
            ToolDefinition::for_args::<DriveReadArgs>(
                "drive_read",
                "Read content of a Google Drive file",
            ),
            ToolDefinition::for_args::<DriveListArgs>(
                "drive_list_files",
                "List Google Drive files with optional folder filtering and pagination",
            ),
            ToolDefinition::for_args::<GoogleAccountArgs>(
                "calendar_list_calendars",
                "List all Google calendars accessible by the user",
            ),
            ToolDefinition::for_args::<CalendarListEventsArgs>(
                "calendar_list_events",
                "List upcoming Google Calendar events when calendar context is relevant for planning, verification, or action.",
            ),
            ToolDefinition::for_args::<CalendarCreateEventArgs>(
                "calendar_create_event",
                "Create a new Google Calendar event",
            ),
            ToolDefinition::for_args::<CalendarUpdateEventArgs>(
                "calendar_update_event",
                "Modify an existing Google Calendar event by exact event ID. Use calendar_list_events first when the event ID is unknown or the user request could match multiple events. Only change fields the user asked to change; unspecified fields remain unchanged. Defaults to not notifying guests unless send_updates is explicitly set.",
            ),
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        tracing::debug!("google.execute: {tool_name}");

        if tool_name == "google_list_accounts" {
            return self.list_accounts().await;
        }

        match tool_name {
            "drive_search" => {
                let args: DriveSearchArgs = serde_json::from_str(arguments)?;
                self.drive_search(&args.account_id, &args.query).await
            }
            "drive_read" => {
                let args: DriveReadArgs = serde_json::from_str(arguments)?;
                self.drive_read(&args.account_id, &args.file_id).await
            }
            "drive_list_files" => {
                let args: DriveListArgs = serde_json::from_str(arguments)?;
                self.drive_list_files(
                    &args.account_id,
                    args.folder_id.as_deref(),
                    args.query.as_deref(),
                    args.page_size,
                    args.page_token.as_deref(),
                )
                .await
            }
            "calendar_list_calendars" => {
                let args: GoogleAccountArgs = serde_json::from_str(arguments)?;
                let cals = self.calendar_list_calendars(&args.account_id).await?;
                Ok(serde_json::to_string_pretty(&cals)?)
            }
            "calendar_list_events" => {
                let args: CalendarListEventsArgs = serde_json::from_str(arguments)?;
                let query = args.query.trim();
                let time_min = args.time_min.trim();
                let query = if query.is_empty() {
                    None
                } else {
                    Some(query.to_string())
                };
                let time_min = if time_min.is_empty() {
                    None
                } else {
                    Some(time_min.to_string())
                };
                let calendar_id = args.calendar_id.filter(|c| !c.trim().is_empty());
                self.calendar_list_events(&args.account_id, calendar_id, query, time_min)
                    .await
            }
            "calendar_create_event" => {
                let args: CalendarCreateEventArgs = serde_json::from_str(arguments)?;
                let description = if args.description.trim().is_empty() {
                    None
                } else {
                    Some(args.description)
                };
                let calendar_id = args.calendar_id.filter(|c| !c.trim().is_empty());
                self.calendar_create_event(
                    &args.account_id,
                    calendar_id,
                    &args.summary,
                    &args.start_time,
                    &args.end_time,
                    description,
                )
                .await
            }
            "calendar_update_event" => {
                let args: CalendarUpdateEventArgs = serde_json::from_str(arguments)?;
                let calendar_id = args.calendar_id.filter(|c| !c.trim().is_empty());
                let update = CalendarEventUpdate {
                    summary: args.summary,
                    start_time: args.start_time,
                    end_time: args.end_time,
                    start_date: args.start_date,
                    end_date: args.end_date,
                    description: args.description,
                    location: args.location,
                };
                self.calendar_update_event(
                    &args.account_id,
                    calendar_id,
                    &args.event_id,
                    update,
                    args.send_updates,
                )
                .await
            }
            _ => anyhow::bail!("Unknown google tool: {tool_name}"),
        }
    }

    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        if let Some(db) = &self.db {
            let accounts = db.list_integration_accounts("google").await?;
            if !accounts.is_empty() {
                return Ok(OnboardingStatus::Configured);
            }
        }

        let redirect_uri = "http://localhost:3000/oauth/callback";
        let url = Self::generate_auth_url(&self.config, redirect_uri, None);

        Ok(OnboardingStatus::RequiresAction {
            fields: vec![OnboardingField {
                name: "refresh_token".to_string(),
                label: "Connect Google Account".to_string(),
                input_type: "oauth".to_string(),
                value: Some(url),
                description: Some("Click to authorize Jossie with Google".to_string()),
            }],
        })
    }

    async fn poll(&self) -> anyhow::Result<()> {
        let Some(db) = &self.db else {
            return Ok(());
        };

        let accounts = db.list_integration_accounts("google").await?;
        for acc in accounts {
            if self.is_account_paused(db, &acc).await? {
                tracing::warn!("Skipping paused Google account {} during poll", acc.id);
                if let Err(e) = self.queue_reconnect_notice_if_due(db, &acc).await {
                    tracing::warn!(
                        "Failed to queue reconnect reminder for paused Google account {}: {e}",
                        acc.id
                    );
                }
                continue;
            }

            if let Err(e) = self.poll_gmail_for_account(db, &acc).await {
                if self.handle_poll_invalid_grant(db, &acc, &e).await? {
                    continue;
                }
                tracing::warn!("Gmail poll failed for account {}: {e}", acc.id);
            }
            if let Err(e) = self.poll_calendar_for_account(db, &acc).await {
                if self.handle_poll_invalid_grant(db, &acc, &e).await? {
                    continue;
                }
                tracing::warn!("Calendar poll failed for account {}: {e}", acc.id);
            }
        }

        Ok(())
    }
}
