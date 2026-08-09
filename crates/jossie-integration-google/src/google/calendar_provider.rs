impl GoogleIntegration {
    pub async fn calendar_list_calendars(
        &self,
        account_id: &str,
    ) -> anyhow::Result<Vec<CalendarListEntry>> {
        let token = self.get_access_token(account_id).await?;
        let mut calendars = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .get("https://www.googleapis.com/calendar/v3/users/me/calendarList")
                .bearer_auth(&token)
                .query(&[("maxResults", "100")]);

            if let Some(ref token) = page_token {
                req = req.query(&[("pageToken", token)]);
            }

            let resp = req.send().await?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Calendar list failed: {body}");
            }

            #[derive(Deserialize)]
            struct CalendarListResp {
                items: Vec<CalendarListEntry>,
                #[serde(rename = "nextPageToken")]
                next_page_token: Option<String>,
            }

            let list: CalendarListResp = resp.json().await?;
            calendars.extend(list.items);

            if let Some(token) = list.next_page_token {
                page_token = Some(token);
            } else {
                break;
            }
        }

        Ok(calendars)
    }

    pub async fn calendar_list_updated_events(
        &self,
        account_id: &str,
        calendar_id: &str,
        updated_min: &str,
    ) -> anyhow::Result<Vec<CalendarEventSummary>> {
        let token = self.get_access_token(account_id).await?;
        let clean_calendar_id = if calendar_id.trim().is_empty() {
            "primary"
        } else {
            calendar_id
        };
        let mut url = reqwest::Url::parse("https://www.googleapis.com/calendar/v3/calendars")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("URL cannot be base"))?
            .push(clean_calendar_id)
            .push("events");

        let resp = self
            .client
            .get(url)
            .bearer_auth(&token)
            .query(&[
                ("maxResults", "50"),
                ("singleEvents", "true"),
                ("orderBy", "updated"),
                ("updatedMin", updated_min),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Calendar updated events failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        let items = data
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut events = Vec::new();
        for item in items {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                continue;
            }
            let summary = item
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled")
                .to_string();
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("confirmed")
                .to_string();
            let updated = item
                .get("updated")
                .and_then(|v| v.as_str())
                .unwrap_or(updated_min)
                .to_string();
            let location = item
                .get("location")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let start = extract_event_time(item.get("start"));
            let end = extract_event_time(item.get("end"));

            events.push(CalendarEventSummary {
                id,
                summary,
                start,
                end,
                status,
                updated,
                location,
            });
        }

        Ok(events)
    }

}
