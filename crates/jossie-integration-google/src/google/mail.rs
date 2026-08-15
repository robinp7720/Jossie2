impl GoogleIntegration {
    pub async fn mail_search(
        &self,
        account_id: &str,
        query: &str,
        max_results: Option<u32>,
        page_token: Option<&str>,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let max_results = max_results.unwrap_or(20).to_string();
        let mut req = self
            .client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(&token)
            .query(&[("q", query), ("maxResults", &max_results)]);

        if let Some(token) = page_token {
            req = req.query(&[("pageToken", token)]);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail search failed: {body}");
        }

        #[derive(Deserialize)]
        struct ListResponse {
            #[serde(default)]
            messages: Vec<MessageRef>,
            #[serde(rename = "nextPageToken")]
            next_page_token: Option<String>,
        }
        #[derive(Clone, Deserialize, Serialize)]
        struct MessageRef {
            id: String,
            #[serde(rename = "threadId")]
            thread_id: String,
        }

        let list: ListResponse = resp.json().await?;

        if list.messages.is_empty() {
            return Ok(serde_json::to_string_pretty(&serde_json::json!({
                "messages": [],
                "next_page_token": list.next_page_token
            }))?);
        }

        // Fetch metadata concurrently, but keep bounded pressure on Gmail and
        // restore provider order before returning results.
        use futures::{StreamExt, stream};
        let client = &self.client;
        let token_ref = &token;
        let mut indexed = stream::iter(list.messages.into_iter().enumerate())
            .map(|(index, msg_ref)| async move {
                let url = format!(
                    "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
                    msg_ref.id
                );
                let response = client
                    .get(&url)
                    .bearer_auth(token_ref)
                    .query(&[
                        ("format", "metadata"),
                        ("metadataHeaders", "From"),
                        ("metadataHeaders", "Subject"),
                        ("metadataHeaders", "Date"),
                    ])
                    .send()
                    .await
                    .ok()?;
                let msg = response.json::<serde_json::Value>().await.ok()?;
                Some((
                    index,
                    serde_json::json!({
                        "id": msg_ref.id,
                        "snippet": msg.get("snippet").and_then(|s| s.as_str()).unwrap_or(""),
                        "headers": msg.pointer("/payload/headers"),
                    }),
                ))
            })
            .buffer_unordered(8)
            .filter_map(|item| async move { item })
            .collect::<Vec<_>>()
            .await;
        indexed.sort_by_key(|(index, _)| *index);
        let results = indexed
            .into_iter()
            .map(|(_, message)| message)
            .collect::<Vec<_>>();

        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "messages": results,
            "next_page_token": list.next_page_token
        }))?)
    }

    pub async fn mail_read(&self, account_id: &str, message_id: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let url = format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}");
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&token)
            .query(&[("format", "full")])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail read failed: {body}");
        }

        let msg: serde_json::Value = resp.json().await?;
        let snippet = msg
            .get("snippet")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        // Extract headers
        let headers = msg
            .pointer("/payload/headers")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();

        let get_header = |name: &str| -> String {
            headers
                .iter()
                .find(|h| h.get("name").and_then(|n| n.as_str()) == Some(name))
                .and_then(|h| h.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        // Extract body - prefer the most informative part (fetch attachment-backed parts if needed)
        let mut body_text = extract_body_from_payload(
            &self.client,
            &token,
            message_id,
            &msg["payload"],
            self.config.debug_gmail_payload,
        )
        .await;
        let mut debug_info = String::new();

        if body_text.trim().is_empty() {
            debug_info = summarize_structure(&msg["payload"], 0);
            tracing::warn!(
                "Empty body for email {}. Structure:\n{}",
                message_id,
                debug_info
            );
            body_text = snippet.clone();
        }

        let attachments = collect_attachments(&msg["payload"]);

        Ok(serde_json::json!({
            "id": message_id,
            "snippet": snippet,
            "from": get_header("From"),
            "to": get_header("To"),
            "subject": get_header("Subject"),
            "date": get_header("Date"),
            "body": body_text,
            "body_source": if debug_info.is_empty() { "full" } else { "snippet" },
            "attachments": attachments,
            "debug_structure": if !debug_info.is_empty() { Some(debug_info) } else { None },
        })
        .to_string())
    }

    pub async fn mail_download_attachment(
        &self,
        account_id: &str,
        message_id: &str,
        attachment_id: &str,
    ) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(!attachment_id.trim().is_empty(), "Missing Gmail attachment ID");
        let token = self.get_access_token(account_id).await?;
        let url = format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}/attachments/{attachment_id}"
        );
        let resp = self.client.get(url).bearer_auth(&token).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail attachment download failed ({status}): {body}");
        }
        let payload: serde_json::Value = resp.json().await?;
        let raw = payload
            .get("data")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow::anyhow!("Gmail attachment response has no data"))?;
        decode_base64_url_bytes(raw)
            .ok_or_else(|| anyhow::anyhow!("Gmail attachment data is not valid base64"))
    }

    pub async fn mail_send(
        &self,
        account_id: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;

        let raw_email = format!(
            "To: {to}\r\nSubject: {subject}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}"
        );
        use base64::Engine;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw_email.as_bytes());

        let resp = self
            .client
            .post("https://gmail.googleapis.com/gmail/v1/users/me/messages/send")
            .bearer_auth(&token)
            .json(&serde_json::json!({"raw": encoded}))
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail send failed: {body}");
        }

        Ok(format!("Email sent to {to}"))
    }

    pub async fn mail_labels(&self, account_id: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let resp = self
            .client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/labels")
            .bearer_auth(&token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gmail label list failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        let labels = data
            .get("labels")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(serde_json::to_string_pretty(&labels)?)
    }

}
