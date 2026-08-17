impl GoogleIntegration {
    async fn poll_gmail_for_account(
        &self,
        db: &Arc<Database>,
        acc: &IntegrationAccount,
    ) -> anyhow::Result<()> {
        let history_key = format!("gmail_history_id:{}", acc.id);
        let history_id = match db.get_integration_setting("google", &history_key).await? {
            Some(val) => val,
            None => {
                let profile = self.gmail_get_profile(&acc.id).await?;
                db.set_integration_setting("google", &history_key, &profile.history_id)
                    .await?;
                return Ok(());
            }
        };

        match self.gmail_list_history(&acc.id, &history_id).await? {
            GmailHistoryOutcome::Reset { history_id } => {
                db.set_integration_setting("google", &history_key, &history_id)
                    .await?;
                return Ok(());
            }
            GmailHistoryOutcome::Updated(result) => {
                let account_email = self.get_account_email(acc);
                for msg in result.messages {
                    tracing::info!("New Gmail message: {}", msg.id);
                    let payload = serde_json::json!({
                        "message_id": msg.id,
                        "message_unique_id": msg.id,
                        "thread_id": msg.thread_id,
                        "from": msg.from,
                        "subject": msg.subject,
                        "date": msg.date,
                        "received_at": msg.received_at,
                        "event_semantics": "new_message_arrival",
                        "snippet": msg.snippet,
                        "account_id": acc.id,
                        "account_email": account_email,
                    });
                    let _ = db
                        .insert_integration_event(
                            "google",
                            &acc.id,
                            GMAIL_NEW_MESSAGE,
                            &msg.id,
                            &payload,
                        )
                        .await?;
                }
                db.set_integration_setting("google", &history_key, &result.history_id)
                    .await?;
            }
        }

        Ok(())
    }

    async fn poll_calendar_for_account(
        &self,
        db: &Arc<Database>,
        acc: &IntegrationAccount,
    ) -> anyhow::Result<()> {
        let calendars = match self.calendar_list_calendars(&acc.id).await {
            Ok(cals) => cals,
            Err(e) => {
                tracing::error!("Failed to list calendars for account {}: {}", acc.id, e);
                return Err(e);
            }
        };

        let account_email = self.get_account_email(acc);

        for calendar in calendars {
            let calendar_id = &calendar.id;
            let updated_key = format!("calendar_updated_min:{}:{}", acc.id, calendar_id);

            let db_key = updated_key.clone();

            let updated_min = match db.get_integration_setting("google", &db_key).await? {
                Some(val) => val,
                None => {
                    // If this is primary, check if we have the old legacy key
                    if calendar.primary {
                        if let Some(val) = db
                            .get_integration_setting(
                                "google",
                                &format!("calendar_updated_min:{}", acc.id),
                            )
                            .await?
                        {
                            val
                        } else {
                            // Default to now
                            let now = Utc::now().to_rfc3339();
                            db.set_integration_setting("google", &db_key, &now).await?;
                            now
                        }
                    } else {
                        let now = Utc::now().to_rfc3339();
                        db.set_integration_setting("google", &db_key, &now).await?;
                        now
                    }
                }
            };

            match self
                .calendar_list_updated_events(&acc.id, calendar_id, &updated_min)
                .await
            {
                Ok(events) => {
                    let mut max_updated = updated_min.clone();
                    for ev in events {
                        if ev.updated > max_updated {
                            max_updated = ev.updated.clone();
                        }
                        let dedupe_key = format!("{}:{}:{}", calendar_id, ev.id, ev.updated);
                        let payload = serde_json::json!({
                            "event_id": ev.id,
                            "calendar_id": calendar_id,
                            "calendar_summary": calendar.summary,
                            "summary": ev.summary,
                            "start": ev.start,
                            "end": ev.end,
                            "status": ev.status,
                            "updated": ev.updated,
                            "location": ev.location,
                            "account_id": acc.id,
                            "account_email": account_email,
                        });
                        let _ = db
                            .insert_integration_event(
                                "google",
                                &acc.id,
                                CALENDAR_EVENT_UPDATED,
                                &dedupe_key,
                                &payload,
                            )
                            .await?;
                    }

                    if max_updated != updated_min {
                        db.set_integration_setting("google", &db_key, &max_updated)
                            .await?;
                    }
                }
                Err(e) => {
                    if e.to_string().contains("updatedMinTooLongAgo") {
                        let reset_to = Utc::now().to_rfc3339();
                        if let Err(set_err) = db
                            .set_integration_setting("google", &db_key, &reset_to)
                            .await
                        {
                            tracing::warn!(
                                "Calendar {} for account {} returned updatedMinTooLongAgo, but failed to reset cursor: {}",
                                calendar_id,
                                acc.id,
                                set_err
                            );
                        } else {
                            tracing::warn!(
                                "Calendar {} for account {} returned updatedMinTooLongAgo; reset updatedMin cursor to {}",
                                calendar_id,
                                acc.id,
                                reset_to
                            );
                        }
                        continue;
                    }

                    tracing::warn!(
                        "Failed to poll calendar {} for account {}: {}",
                        calendar_id,
                        acc.id,
                        e
                    );
                }
            }
        }

        Ok(())
    }

    fn get_account_email(&self, acc: &IntegrationAccount) -> Option<String> {
        serde_json::from_str::<StoredAccount>(&acc.data)
            .ok()
            .map(|data| data.email)
    }
}
