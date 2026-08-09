impl HttpIntegration {
    pub fn new(allowed_domains: Vec<String>) -> Self {
        Self { allowed_domains }
    }

    // Redact secrets in logging
    fn redact_headers(&self, headers: &HeaderMap) -> String {
        let mut debug_map = HashMap::new();
        for (k, v) in headers {
            let key = k.as_str().to_lowercase();
            let val_str = v.to_str().unwrap_or("<binary>");

            if key.contains("auth") || key.contains("cookie") || key.contains("key") {
                debug_map.insert(key, "[REDACTED]");
            } else {
                debug_map.insert(key, val_str);
            }
        }
        format!("{:?}", debug_map)
    }

    // Log complete request details for debugging
    fn log_request_details(
        &self,
        method: &reqwest::Method,
        url: &Url,
        headers: &HeaderMap,
        body_bytes: &Option<Vec<u8>>,
        multipart: bool,
    ) {
        let separator = "=".repeat(80);
        tracing::info!("{}", separator);
        tracing::info!("📤 OUTGOING HTTP REQUEST");
        tracing::info!("{}", separator);
        tracing::info!("Method: {}", method);
        tracing::info!("URL: {}", url);

        if let Some(query) = url.query() {
            tracing::info!("Query String: {}", query);
        }

        tracing::info!(
            "Headers ({}): {}",
            headers.len(),
            self.redact_headers(headers)
        );

        if multipart {
            tracing::info!("Body: [multipart/form-data - cannot display structure]");
        } else if let Some(bytes) = body_bytes {
            let body_size = bytes.len();
            tracing::info!("Body Size: {} bytes", body_size);

            if body_size == 0 {
                tracing::info!("Body: <empty>");
            } else if body_size > 1000 {
                // Truncate large bodies
                if let Ok(text) = std::str::from_utf8(&bytes[..1000.min(body_size)]) {
                    tracing::info!("Body (first 1000 bytes): {}", text);
                    tracing::info!("Body: ... truncated {} bytes ...", body_size - 1000);
                } else {
                    tracing::info!("Body: <binary data, {} bytes>", body_size);
                }
            } else {
                // Log full body for small requests
                if let Ok(text) = std::str::from_utf8(bytes) {
                    tracing::info!("Body: {}", text);
                } else {
                    tracing::info!("Body: <binary data, {} bytes>", body_size);
                }
            }
        } else {
            tracing::info!("Body: <none>");
        }
        tracing::info!("{}", separator);
    }

    // Log complete response details for debugging
    fn log_response_details(
        &self,
        status: reqwest::StatusCode,
        headers: &HashMap<String, String>,
        body_text: &str,
    ) {
        let separator = "=".repeat(80);
        tracing::info!("{}", separator);
        tracing::info!("📥 INCOMING HTTP RESPONSE");
        tracing::info!("{}", separator);
        tracing::info!(
            "Status: {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        );
        tracing::info!("Headers ({}): {:?}", headers.len(), headers);

        let body_size = body_text.len();
        tracing::info!("Body Size: {} bytes", body_size);

        if body_size == 0 {
            tracing::info!("Body: <empty>");
        } else if body_size > 2000 {
            // Truncate large responses
            tracing::info!("Body (first 2000 chars): {}", &body_text[..2000]);
            tracing::info!("Body: ... truncated {} chars ...", body_size - 2000);
        } else {
            tracing::info!("Body: {}", body_text);
        }
        tracing::info!("{}", separator);
    }

}
