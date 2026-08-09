impl LlmClient {
    pub fn new(api_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_url: api_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            reasoning_effort: None,
            reasoning_context: None,
            enable_web_search: false,
            service_tier: Some("flex".to_string()),
            transcription_model: Some("gpt-transcribe".to_string()),
            max_attachment_bytes_per_request: 25 * 1024 * 1024,
        }
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }

    pub fn set_reasoning_context(&mut self, context: Option<String>) {
        self.reasoning_context = context;
    }

    pub fn set_enable_web_search(&mut self, enabled: bool) {
        self.enable_web_search = enabled;
    }

    pub fn set_service_tier(&mut self, service_tier: Option<String>) {
        self.service_tier = service_tier;
    }

    pub fn set_transcription_model(&mut self, transcription_model: Option<String>) {
        self.transcription_model = transcription_model.filter(|model| !model.trim().is_empty());
    }

    pub fn set_max_attachment_bytes_per_request(&mut self, max_bytes: usize) {
        self.max_attachment_bytes_per_request = max_bytes;
    }

    pub fn transcription_is_configured(&self) -> bool {
        self.transcription_model.is_some()
    }

    pub async fn transcribe_file(
        &self,
        path: &Path,
        filename: &str,
        mime_type: &str,
    ) -> anyhow::Result<String> {
        let model = self
            .transcription_model
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Audio transcription is disabled"))?;
        let bytes = tokio::fs::read(path).await?;
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(mime_type)?;
        let form = reqwest::multipart::Form::new()
            .text("model", model.to_string())
            .part("file", part);
        let response = self
            .client
            .post(format!("{}/audio/transcriptions", self.api_url))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Transcription API error {status}: {body}");
        }
        let value: Value = response.json().await?;
        value
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Transcription response did not contain text"))
    }

}
