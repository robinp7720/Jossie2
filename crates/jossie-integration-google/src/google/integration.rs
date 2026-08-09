#[async_trait::async_trait]
impl Integration for GoogleIntegration {
    fn name(&self) -> &str {
        "google"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "google_list_accounts".to_string(),
                description: "List configured Google accounts".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "drive_search".to_string(),
                description: "Search Google Drive files by name".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "query": {"type": "string", "description": "Search term for file names"}
                    },
                    "required": ["account_id", "query"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "drive_read".to_string(),
                description: "Read content of a Google Drive file".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "file_id": {"type": "string", "description": "Google Drive file ID from search results"}
                    },
                    "required": ["account_id", "file_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "drive_list_files".to_string(),
                description:
                    "List Google Drive files with optional folder filtering and pagination"
                        .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "folder_id": {"type": ["string", "null"], "description": "Optional folder ID to list files within (null for root or all files)"},
                        "query": {"type": ["string", "null"], "description": "Optional search query to filter files by name"},
                        "page_size": {"type": ["integer", "null"], "description": "Number of files to return (default: 20, max: 1000)"},
                        "page_token": {"type": ["string", "null"], "description": "Token for pagination from previous results"}
                    },
                    "required": ["account_id", "folder_id", "query", "page_size", "page_token"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "calendar_list_calendars".to_string(),
                description: "List all Google calendars accessible by the user".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"}
                    },
                    "required": ["account_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "calendar_list_events".to_string(),
                description: "List upcoming Google Calendar events when calendar context is relevant for planning, verification, or action.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "calendar_id": {"type": ["string", "null"], "description": "Calendar ID (optional, defaults to primary)"},
                        "query": {"type": "string", "description": "Filter events by text query (use empty string for none)"},
                        "time_min": {"type": "string", "description": "Start time (ISO 8601) to list events from (use empty string for now)"}
                    },
                    "required": ["account_id", "query", "time_min", "calendar_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "calendar_create_event".to_string(),
                description: "Create a new Google Calendar event".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "calendar_id": {"type": ["string", "null"], "description": "Calendar ID (optional, defaults to primary)"},
                        "summary": {"type": "string", "description": "Event title"},
                        "start_time": {"type": "string", "description": "Start time (ISO 8601)"},
                        "end_time": {"type": "string", "description": "End time (ISO 8601)"},
                        "description": {"type": "string", "description": "Event description (use empty string for none)"}
                    },
                    "required": ["account_id", "summary", "start_time", "end_time", "description", "calendar_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "calendar_update_event".to_string(),
                description: "Modify an existing Google Calendar event by exact event ID. Use calendar_list_events first when the event ID is unknown or the user request could match multiple events. Only change fields the user asked to change; unspecified fields remain unchanged. Defaults to not notifying guests unless send_updates is explicitly set.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Account ID from google_list_accounts"},
                        "calendar_id": {"type": ["string", "null"], "description": "Calendar ID (optional, defaults to primary)"},
                        "event_id": {"type": "string", "description": "Exact Google Calendar event ID from calendar_list_events"},
                        "summary": {"type": ["string", "null"], "description": "New event title, or null to leave unchanged"},
                        "start_time": {"type": ["string", "null"], "description": "New timed-event start time (ISO 8601), paired with end_time; use null to leave unchanged"},
                        "end_time": {"type": ["string", "null"], "description": "New timed-event end time (ISO 8601), paired with start_time; use null to leave unchanged"},
                        "start_date": {"type": ["string", "null"], "description": "New all-day start date (YYYY-MM-DD), paired with end_date; use null to leave unchanged"},
                        "end_date": {"type": ["string", "null"], "description": "New all-day exclusive end date (YYYY-MM-DD), paired with start_date; use null to leave unchanged"},
                        "description": {"type": ["string", "null"], "description": "New event description; use empty string to clear or null to leave unchanged"},
                        "location": {"type": ["string", "null"], "description": "New event location; use empty string to clear or null to leave unchanged"},
                        "send_updates": {"type": ["string", "null"], "enum": ["all", "externalOnly", "none", null], "description": "Guest notification behavior; defaults to none"}
                    },
                    "required": ["account_id", "calendar_id", "event_id", "summary", "start_time", "end_time", "start_date", "end_date", "description", "location", "send_updates"],
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        tracing::debug!("google.execute: {tool_name}");

        if tool_name == "google_list_accounts" {
            return self.list_accounts().await;
        }

        match tool_name {
            "drive_search" => {
                #[derive(Deserialize)]
                struct Args {
                    query: String,
                    account_id: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.drive_search(&args.account_id, &args.query).await
            }
            "drive_read" => {
                #[derive(Deserialize)]
                struct Args {
                    file_id: String,
                    account_id: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.drive_read(&args.account_id, &args.file_id).await
            }
            "drive_list_files" => {
                #[derive(Deserialize)]
                struct Args {
                    account_id: String,
                    folder_id: Option<String>,
                    query: Option<String>,
                    page_size: Option<u32>,
                    page_token: Option<String>,
                }
                let args: Args = serde_json::from_str(arguments)?;
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
                #[derive(Deserialize)]
                struct Args {
                    account_id: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let cals = self.calendar_list_calendars(&args.account_id).await?;
                Ok(serde_json::to_string_pretty(&cals)?)
            }
            "calendar_list_events" => {
                #[derive(Deserialize)]
                struct Args {
                    query: String,
                    time_min: String,
                    account_id: String,
                    calendar_id: Option<String>,
                }
                let args: Args = serde_json::from_str(arguments)?;
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
                #[derive(Deserialize)]
                struct Args {
                    summary: String,
                    start_time: String,
                    end_time: String,
                    description: String,
                    account_id: String,
                    calendar_id: Option<String>,
                }
                let args: Args = serde_json::from_str(arguments)?;
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
                #[derive(Deserialize)]
                struct Args {
                    account_id: String,
                    calendar_id: Option<String>,
                    event_id: String,
                    summary: Option<String>,
                    start_time: Option<String>,
                    end_time: Option<String>,
                    start_date: Option<String>,
                    end_date: Option<String>,
                    description: Option<String>,
                    location: Option<String>,
                    send_updates: Option<String>,
                }
                let args: Args = serde_json::from_str(arguments)?;
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
