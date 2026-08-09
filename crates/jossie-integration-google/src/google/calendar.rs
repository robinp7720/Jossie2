impl GoogleIntegration {
    async fn calendar_list_events(
        &self,
        account_id: &str,
        calendar_id: Option<String>,
        query: Option<String>,
        time_min: Option<String>,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let calendar_id = calendar_id.unwrap_or_else(|| "primary".to_string());
        let mut url = reqwest::Url::parse("https://www.googleapis.com/calendar/v3/calendars")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("URL cannot be base"))?
            .push(&calendar_id)
            .push("events");

        let mut req = self.client.get(url).bearer_auth(&token).query(&[
            ("maxResults", "10"),
            ("singleEvents", "true"),
            ("orderBy", "startTime"),
        ]);

        if let Some(q) = query {
            req = req.query(&[("q", q)]);
        }

        let tm = time_min.unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        req = req.query(&[("timeMin", tm)]);

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Calendar list failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        let events = data.get("items").unwrap_or(&serde_json::json!([])).clone();

        Ok(serde_json::to_string_pretty(&events)?)
    }

    async fn calendar_create_event(
        &self,
        account_id: &str,
        calendar_id: Option<String>,
        summary: &str,
        start_time: &str,
        end_time: &str,
        description: Option<String>,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let calendar_id = calendar_id.unwrap_or_else(|| "primary".to_string());
        let mut url = reqwest::Url::parse("https://www.googleapis.com/calendar/v3/calendars")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("URL cannot be base"))?
            .push(&calendar_id)
            .push("events");

        let body = serde_json::json!({
            "summary": summary,
            "description": description.unwrap_or_default(),
            "start": { "dateTime": start_time },
            "end": { "dateTime": end_time }
        });

        let resp = self
            .client
            .post(url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Calendar create failed: {body}");
        }

        let event: serde_json::Value = resp.json().await?;
        Ok(serde_json::to_string_pretty(&event)?)
    }

    async fn calendar_update_event(
        &self,
        account_id: &str,
        calendar_id: Option<String>,
        event_id: &str,
        update: CalendarEventUpdate,
        send_updates: Option<String>,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let event_id = event_id.trim();
        if event_id.is_empty() {
            anyhow::bail!("event_id is required");
        }

        let calendar_id = calendar_id.unwrap_or_else(|| "primary".to_string());
        let mut url = reqwest::Url::parse("https://www.googleapis.com/calendar/v3/calendars")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("URL cannot be base"))?
            .push(&calendar_id)
            .push("events")
            .push(event_id);

        let body = build_calendar_update_body(update)?;
        let send_updates = normalize_calendar_send_updates(send_updates.as_deref())?;

        let mut req = self.client.patch(url).bearer_auth(&token).json(&body);
        if let Some(send_updates) = send_updates.as_deref() {
            req = req.query(&[("sendUpdates", send_updates)]);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Calendar update failed: {body}");
        }

        let event: serde_json::Value = resp.json().await?;
        Ok(serde_json::to_string_pretty(&event)?)
    }

}
