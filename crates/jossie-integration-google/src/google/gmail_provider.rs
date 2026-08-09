impl GoogleIntegration {
    pub async fn gmail_get_profile(&self, account_id: &str) -> anyhow::Result<GmailProfile> {
        let token = self.get_access_token(account_id).await?;
        let resp = self
            .client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/profile")
            .bearer_auth(&token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail profile fetch failed: {body}");
        }

        #[derive(Deserialize)]
        struct ProfileResp {
            #[serde(rename = "historyId")]
            history_id: String,
        }

        let profile: ProfileResp = resp.json().await?;
        Ok(GmailProfile {
            history_id: profile.history_id,
        })
    }

    pub async fn gmail_list_history(
        &self,
        account_id: &str,
        start_history_id: &str,
    ) -> anyhow::Result<GmailHistoryOutcome> {
        let token = self.get_access_token(account_id).await?;
        let mut page_token: Option<String> = None;
        let mut seen_message_ids: HashSet<String> = HashSet::new();
        let mut message_ids: Vec<String> = Vec::new();
        let mut latest_history_id: Option<String> = None;

        loop {
            let mut req = self
                .client
                .get("https://gmail.googleapis.com/gmail/v1/users/me/history")
                .bearer_auth(&token)
                .query(&[
                    ("startHistoryId", start_history_id),
                    ("historyTypes", "messageAdded"),
                    ("maxResults", "100"),
                ]);

            if let Some(ref token) = page_token {
                req = req.query(&[("pageToken", token)]);
            }

            let resp = req.send().await?;
            if resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::BAD_REQUEST {
                let profile = self.gmail_get_profile(account_id).await?;
                return Ok(GmailHistoryOutcome::Reset {
                    history_id: profile.history_id,
                });
            }

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Gmail history list failed: {body}");
            }

            #[derive(Deserialize)]
            struct HistoryList {
                #[serde(rename = "historyId")]
                history_id: Option<String>,
                #[serde(rename = "nextPageToken")]
                next_page_token: Option<String>,
                #[serde(default)]
                history: Vec<HistoryItem>,
            }

            #[derive(Deserialize)]
            struct HistoryItem {
                #[serde(rename = "messagesAdded")]
                messages_added: Option<Vec<MessageAdded>>,
            }

            #[derive(Deserialize)]
            struct MessageAdded {
                message: Option<MessageRef>,
            }

            #[derive(Deserialize)]
            struct MessageRef {
                id: String,
            }

            let list: HistoryList = resp.json().await?;
            if let Some(hid) = list.history_id {
                latest_history_id = Some(hid);
            }

            for item in list.history {
                if let Some(added) = item.messages_added {
                    for entry in added {
                        if let Some(message) = entry.message {
                            if seen_message_ids.insert(message.id.clone()) {
                                message_ids.push(message.id);
                            }
                        }
                    }
                }
            }

            if let Some(next) = list.next_page_token {
                page_token = Some(next);
            } else {
                break;
            }
        }

        let mut messages = Vec::new();
        for message_id in message_ids {
            if let Ok(summary) = self
                .gmail_fetch_message_summary(account_id, &message_id)
                .await
            {
                messages.push(summary);
            }
        }
        messages.sort_by(|a, b| {
            a.internal_ts_ms
                .cmp(&b.internal_ts_ms)
                .then_with(|| a.id.cmp(&b.id))
        });

        Ok(GmailHistoryOutcome::Updated(GmailHistoryPollResult {
            history_id: latest_history_id.unwrap_or_else(|| start_history_id.to_string()),
            messages,
        }))
    }

    async fn gmail_fetch_message_summary(
        &self,
        account_id: &str,
        message_id: &str,
    ) -> anyhow::Result<GmailMessageSummary> {
        let token = self.get_access_token(account_id).await?;
        let resp = self
            .client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}"
            ))
            .bearer_auth(&token)
            .query(&[
                ("format", "metadata"),
                ("metadataHeaders", "From"),
                ("metadataHeaders", "Subject"),
                ("metadataHeaders", "Date"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail message fetch failed: {body}");
        }

        #[derive(Deserialize)]
        struct MessageResp {
            id: String,
            #[serde(rename = "threadId")]
            thread_id: String,
            #[serde(rename = "internalDate")]
            internal_date: Option<String>,
            snippet: Option<String>,
            payload: Option<MessagePayload>,
        }

        #[derive(Deserialize)]
        struct MessagePayload {
            #[serde(default)]
            headers: Vec<MessageHeader>,
        }

        #[derive(Deserialize)]
        struct MessageHeader {
            name: String,
            value: String,
        }

        let msg: MessageResp = resp.json().await?;
        let headers = msg.payload.map(|p| p.headers).unwrap_or_default();
        let header_value = |name: &str| {
            headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case(name))
                .map(|h| h.value.clone())
                .unwrap_or_default()
        };

        let internal_ts_ms_opt = msg
            .internal_date
            .as_deref()
            .and_then(|v| v.parse::<i64>().ok());
        let received_at = internal_ts_ms_opt
            .and_then(chrono::DateTime::<Utc>::from_timestamp_millis)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| Utc::now().to_rfc3339());

        Ok(GmailMessageSummary {
            id: msg.id,
            thread_id: msg.thread_id,
            from: header_value("From"),
            subject: header_value("Subject"),
            date: header_value("Date"),
            snippet: msg.snippet.unwrap_or_default(),
            received_at,
            internal_ts_ms: internal_ts_ms_opt.unwrap_or(0),
        })
    }

}
