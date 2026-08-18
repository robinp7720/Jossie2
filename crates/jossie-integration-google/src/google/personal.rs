impl GoogleIntegration {
    pub async fn task_lists(&self, account_id: &str) -> anyhow::Result<serde_json::Value> {
        let token = self.get_access_token(account_id).await?;
        let response = self.client
            .get("https://tasks.googleapis.com/tasks/v1/users/@me/lists")
            .bearer_auth(token)
            .query(&[("maxResults", "100")])
            .send().await?;
        google_json(response, "Google Tasks list request").await
    }

    pub async fn tasks_list(&self, account_id: &str, list_id: &str, show_completed: bool) -> anyhow::Result<serde_json::Value> {
        let token = self.get_access_token(account_id).await?;
        let url = format!("https://tasks.googleapis.com/tasks/v1/lists/{}/tasks", urlencoding::encode(list_id));
        let response = self.client.get(url).bearer_auth(token)
            .query(&[("maxResults", "100"), ("showCompleted", if show_completed { "true" } else { "false" }), ("showHidden", "false")])
            .send().await?;
        google_json(response, "Google Tasks request").await
    }

    pub async fn task_create(&self, account_id: &str, list_id: &str, title: &str, notes: Option<&str>, due: Option<&str>) -> anyhow::Result<serde_json::Value> {
        let token = self.get_access_token(account_id).await?;
        let url = format!("https://tasks.googleapis.com/tasks/v1/lists/{}/tasks", urlencoding::encode(list_id));
        let mut body = serde_json::json!({"title": title});
        if let Some(notes) = notes { body["notes"] = serde_json::Value::String(notes.to_string()); }
        if let Some(due) = due { body["due"] = serde_json::Value::String(due.to_string()); }
        let response = self.client.post(url).bearer_auth(token).json(&body).send().await?;
        google_json(response, "Google task creation").await
    }

    pub async fn task_patch(&self, account_id: &str, list_id: &str, task_id: &str, patch: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let token = self.get_access_token(account_id).await?;
        let url = format!("https://tasks.googleapis.com/tasks/v1/lists/{}/tasks/{}", urlencoding::encode(list_id), urlencoding::encode(task_id));
        let response = self.client.patch(url).bearer_auth(token).json(&patch).send().await?;
        google_json(response, "Google task update").await
    }

    async fn contacts_search(&self, account_id: &str, query: &str, page_size: Option<u32>) -> anyhow::Result<String> {
        anyhow::ensure!(!query.trim().is_empty(), "query is required");
        let token = self.get_access_token(account_id).await?;
        let page_size = page_size.unwrap_or(10).clamp(1, 30).to_string();
        let response = self.client.get("https://people.googleapis.com/v1/people:searchContacts")
            .bearer_auth(token)
            .query(&[("query", query), ("readMask", "names,emailAddresses,phoneNumbers,organizations,birthdays,metadata"), ("pageSize", &page_size)])
            .send().await?;
        let value = google_json(response, "Google contact search").await?;
        Ok(serde_json::to_string_pretty(&value)?)
    }

    async fn contact_read(&self, account_id: &str, resource_name: &str) -> anyhow::Result<String> {
        anyhow::ensure!(resource_name.starts_with("people/"), "resource_name must start with people/");
        let token = self.get_access_token(account_id).await?;
        let url = format!("https://people.googleapis.com/v1/{resource_name}");
        let response = self.client.get(url).bearer_auth(token)
            .query(&[("personFields", "names,emailAddresses,phoneNumbers,organizations,birthdays,addresses,relations,urls,metadata")])
            .send().await?;
        let value = google_json(response, "Google contact read").await?;
        Ok(serde_json::to_string_pretty(&value)?)
    }
}

async fn google_json(response: reqwest::Response, operation: &str) -> anyhow::Result<serde_json::Value> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() { anyhow::bail!("{operation} failed ({status}): {body}"); }
    Ok(serde_json::from_str(&body)?)
}
