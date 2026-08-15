impl GoogleIntegration {
    async fn drive_search(&self, account_id: &str, query: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let q = format!("name contains '{}'", query.replace('\'', "\\'"));
        let resp = self
            .client
            .get("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(&token)
            .query(&[
                ("q", &q),
                ("pageSize", &"20".to_string()),
                (
                    "fields",
                    &"files(id,name,mimeType,modifiedTime)".to_string(),
                ),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Drive search failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(serde_json::to_string_pretty(&data["files"])?)
    }

    async fn drive_read(&self, account_id: &str, file_id: &str) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;

        // First get file metadata to check mime type
        let meta_url = format!("https://www.googleapis.com/drive/v3/files/{file_id}");
        let meta_resp = self
            .client
            .get(&meta_url)
            .bearer_auth(&token)
            .query(&[("fields", "id,name,mimeType,size")])
            .send()
            .await?;

        let meta: serde_json::Value = meta_resp.json().await?;
        let mime = meta.get("mimeType").and_then(|m| m.as_str()).unwrap_or("");

        // For Google Docs, export as plain text
        let content = if mime.starts_with("application/vnd.google-apps.") {
            let export_mime = match mime {
                "application/vnd.google-apps.document" => "text/plain",
                "application/vnd.google-apps.spreadsheet" => "text/csv",
                "application/vnd.google-apps.presentation" => "text/plain",
                _ => "text/plain",
            };
            let export_url = format!("https://www.googleapis.com/drive/v3/files/{file_id}/export");
            let resp = self
                .client
                .get(&export_url)
                .bearer_auth(&token)
                .query(&[("mimeType", export_mime)])
                .send()
                .await?;
            resp.text().await?
        } else {
            // For binary files, just return metadata
            return Ok(serde_json::json!({
                "id": file_id,
                "name": meta.get("name"),
                "mimeType": mime,
                "note": "Binary file - download not supported via chat. Use Drive UI."
            })
            .to_string());
        };

        Ok(serde_json::json!({
            "id": file_id,
            "name": meta.get("name"),
            "mimeType": mime,
            "content": content,
        })
        .to_string())
    }

    async fn drive_list_files(
        &self,
        account_id: &str,
        folder_id: Option<&str>,
        query: Option<&str>,
        page_size: Option<u32>,
        page_token: Option<&str>,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token(account_id).await?;
        let page_size = page_size.unwrap_or(20).min(1000).to_string();

        // Build the query string
        let mut q_parts = Vec::new();

        // If folder_id is specified, filter by parent
        if let Some(fid) = folder_id
            && !fid.trim().is_empty()
        {
            q_parts.push(format!("'{}' in parents", fid.replace("'", "\\'").trim()));
        }

        // Add trashed filter
        q_parts.push("trashed = false".to_string());

        // If query is specified, add name search
        if let Some(q) = query
            && !q.trim().is_empty()
        {
            q_parts.push(format!("name contains '{}'", q.replace("'", "\\'").trim()));
        }

        let full_query = q_parts.join(" and ");

        let mut req = self
            .client
            .get("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(&token)
            .query(&[
                ("q", &full_query),
                ("pageSize", &page_size),
                (
                    "fields",
                    &"nextPageToken,files(id,name,mimeType,size,modifiedTime,webViewLink,parents)"
                        .to_string(),
                ),
                ("orderBy", &"folder,name".to_string()),
            ]);

        if let Some(token) = page_token {
            req = req.query(&[("pageToken", token)]);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Drive list files failed: {body}");
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(serde_json::to_string_pretty(&data)?)
    }

}
