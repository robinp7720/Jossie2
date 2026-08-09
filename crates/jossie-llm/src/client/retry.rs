const MAX_RATE_LIMIT_RETRIES: usize = 5;
const BASE_RETRY_DELAY_SECS: u64 = 2;
const MAX_RETRY_DELAY_SECS: u64 = 60;

impl LlmClient {
    async fn send_responses_request(
        &self,
        request: &ResponsesRequest,
    ) -> anyhow::Result<reqwest::Response> {
        for retry_count in 0..=MAX_RATE_LIMIT_RETRIES {
            let response = self
                .client
                .post(format!("{}/responses", self.api_url))
                .bearer_auth(&self.api_key)
                .json(request)
                .send()
                .await?;

            if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS
                || retry_count == MAX_RATE_LIMIT_RETRIES
            {
                return Ok(response);
            }

            let delay = rate_limit_retry_delay(response.headers(), retry_count);
            let request_id = response
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned);
            let response_body = response.text().await.unwrap_or_default();

            tracing::warn!(
                retry = retry_count + 1,
                max_retries = MAX_RATE_LIMIT_RETRIES,
                delay_ms = delay.as_millis() as u64,
                request_id,
                response_body = %truncate_retry_body(&response_body),
                "LLM API rate limited the request; retrying"
            );
            tokio::time::sleep(delay).await;
        }

        unreachable!("the bounded rate-limit retry loop always returns")
    }
}

fn rate_limit_retry_delay(
    headers: &reqwest::header::HeaderMap,
    retry_count: usize,
) -> std::time::Duration {
    retry_after_delay(headers)
        .unwrap_or_else(|| exponential_retry_delay(retry_count))
        .min(std::time::Duration::from_secs(MAX_RETRY_DELAY_SECS))
}

fn retry_after_delay(
    headers: &reqwest::header::HeaderMap,
) -> Option<std::time::Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;

    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(std::time::Duration::from_secs(seconds));
    }

    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(
        retry_at
            .duration_since(std::time::SystemTime::now())
            .unwrap_or_default(),
    )
}

fn exponential_retry_delay(retry_count: usize) -> std::time::Duration {
    let exponent = u32::try_from(retry_count).unwrap_or(u32::MAX).min(5);
    let seconds = BASE_RETRY_DELAY_SECS
        .saturating_mul(2_u64.saturating_pow(exponent))
        .min(MAX_RETRY_DELAY_SECS);
    let jitter_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::from(duration.subsec_millis()))
        .unwrap_or_default();

    std::time::Duration::from_secs(seconds) + std::time::Duration::from_millis(jitter_ms)
}

fn truncate_retry_body(body: &str) -> String {
    const MAX_CHARS: usize = 500;
    let mut chars = body.chars();
    let truncated: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}
