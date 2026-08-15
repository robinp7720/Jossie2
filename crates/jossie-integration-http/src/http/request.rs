impl HttpIntegration {
    #[allow(clippy::too_many_arguments)]
    async fn http_request(
        &self,
        method: &str,
        url_str: &str,
        headers: Option<HashMap<String, String>>,
        query: Option<HashMap<String, Value>>,
        body: BodyContent,
        timeout_ms: Option<u64>,
        follow_redirects: bool,
    ) -> anyhow::Result<String> {
        tracing::info!("Starting HTTP request: {} {}", method, url_str);
        tracing::debug!(
            "Request params - timeout_ms: {:?}, follow_redirects: {}, has_headers: {}, has_query: {}",
            timeout_ms,
            follow_redirects,
            headers.is_some(),
            query.is_some()
        );

        let url = Url::parse(url_str).map_err(|e| {
            tracing::error!("Failed to parse URL '{}': {}", url_str, e);
            anyhow::anyhow!("Invalid URL: {}", e)
        })?;

        validate_url_target(&url).await?;
        tracing::debug!("URL passed SSRF validation: {}", url);

        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| {
            tracing::error!("Invalid HTTP method: {}", method);
            anyhow::anyhow!("Invalid HTTP method")
        })?;

        // 1. Prepare Client
        // We handle redirects manually for security if needed, but the requirements say:
        // "Must request headers, especially Authorization... Default follow_redirects=false."
        // "If redirects enabled, carry headers only when redirect stays on same origin"

        let client_builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(
                timeout_ms.unwrap_or(20000),
            ))
            .redirect(reqwest::redirect::Policy::none()); // We handle manual redirect to strip auth

        let client = client_builder.build()?;

        // Loops for redirect handling
        let mut current_url = url.clone();
        let mut current_method = method.clone();
        let mut attempts = 0;
        let max_attempts = if follow_redirects { 10 } else { 1 };

        // Prepare initial headers
        let mut final_headers = HeaderMap::new();
        if let Some(h_map) = headers {
            for (k, v) in h_map {
                if let (Ok(hn), Ok(hv)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(&v),
                ) {
                    final_headers.insert(hn, hv);
                } else {
                    tracing::warn!("Failed to parse header: {} = {}", k, v);
                }
            }
        }

        // Log authentication header presence for troubleshooting
        if final_headers.contains_key(reqwest::header::AUTHORIZATION) {
            tracing::debug!("Request includes Authorization header");
        } else {
            tracing::debug!("Request does NOT include Authorization header");
        }
        if final_headers.contains_key(reqwest::header::COOKIE) {
            tracing::debug!("Request includes Cookie header");
        }

        // Apply query params to initial URL
        if let Some(q) = query {
            // We can't easily append to existing query without parsing, so we use pairs
            // But existing pairs might exist.
            // Let's use url's query_pairs_mut
            {
                let mut pairs = current_url.query_pairs_mut();
                for (k, v) in q {
                    let val_str = match v {
                        Value::String(s) => s,
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => continue,
                    };
                    pairs.append_pair(&k, &val_str);
                }
            }
        }

        let mut current_body_bytes: Option<Vec<u8>> = None;
        let mut current_multipart: Option<reqwest::multipart::Form> = None;

        match body {
            BodyContent::None => {
                tracing::debug!("Request has no body");
            }
            BodyContent::Text(s) => {
                tracing::debug!("Request body type: text, size: {} bytes", s.len());
                current_body_bytes = Some(s.into_bytes());
            }
            BodyContent::Json(bytes) => {
                tracing::debug!("Request body type: JSON, size: {} bytes", bytes.len());
                // Auto-set content type if missing
                if !final_headers.contains_key(reqwest::header::CONTENT_TYPE) {
                    final_headers.insert(
                        reqwest::header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                }
                current_body_bytes = Some(bytes);
            }
            BodyContent::Multipart(form) => {
                tracing::debug!("Request body type: multipart/form-data");
                // reqwest handles content-type boundary
                // If caller set content-type, we should probably remove it so reqwest sets it correctly
                final_headers.remove(reqwest::header::CONTENT_TYPE);
                current_multipart = Some(form);
            }
        }

        // Redirect loop
        loop {
            // SECURITY CHECK: Domain Allowlist for Secrets
            // If we have Authorization header, check if domain is allowed.
            if final_headers.contains_key(reqwest::header::AUTHORIZATION)
                && let Some(host) = current_url.host_str()
            {
                    let host_lower = host.to_lowercase();
                    // If allowlist is empty, we allow everything (per user request).
                    // If allowlist is NOT empty, we check if domain is in it or if "*" is present.
                    let mut allowed = self.allowed_domains.is_empty();

                    if !allowed {
                        for allowed_domain in &self.allowed_domains {
                            if allowed_domain == "*" {
                                allowed = true;
                                break;
                            }
                            if host_lower == *allowed_domain
                                || host_lower.ends_with(&format!(".{}", allowed_domain))
                            {
                                allowed = true;
                                break;
                            }
                        }
                    }

                    if !allowed {
                        // Strip or Block?
                        // "Any attempt to send secrets to non-allowlisted domains must be blocked"
                        tracing::warn!(
                            "Blocked: Authentication header present but domain '{}' is not in allowed_domains list",
                            host
                        );
                        return Err(anyhow::anyhow!(
                            "Authentication header present but domain '{}' is not in allowed_domains list.",
                            host
                        ));
                    } else {
                        tracing::debug!("Domain '{}' is in allowed_domains list for auth", host);
                    }
            }

            // Log complete request details
            self.log_request_details(
                &current_method,
                &current_url,
                &final_headers,
                &current_body_bytes,
                current_multipart.is_some(),
            );

            let mut req_builder = client
                .request(current_method.clone(), current_url.clone())
                .headers(final_headers.clone());

            if let Some(bytes) = &current_body_bytes {
                // GET usually doesn't have body, but we allow it if user provided it
                req_builder = req_builder.body(bytes.clone());
            } else if let Some(form) = current_multipart.take() {
                req_builder = req_builder.multipart(form);
            }

            let resp = req_builder.send().await.map_err(|e| {
                tracing::error!(
                    "HTTP request failed for {} {}: {}",
                    current_method,
                    current_url,
                    e
                );
                e
            })?;
            let status = resp.status();
            tracing::info!(
                "Received response: {} from {} {}",
                status,
                current_method,
                current_url
            );

            // Check if redirect
            if status.is_redirection()
                && follow_redirects
                && attempts < max_attempts
                && let Some(location) = resp.headers().get("Location")
                && let Ok(loc_str) = location.to_str()
            {
                        let next_url = match Url::parse(loc_str) {
                            Ok(u) => u,
                            Err(url::ParseError::RelativeUrlWithoutBase) => {
                                // Handle relative
                                current_url.join(loc_str)?
                            }
                            Err(e) => {
                                return Err(anyhow::anyhow!("Invalid redirect location: {}", e));
                            }
                        };

                        // Check Cross-Origin
                        let same_origin = next_url.origin() == current_url.origin();

                        if !same_origin {
                            // STRIP Authorization
                            tracing::warn!(
                                "Redirecting cross-origin from {} to {}. Stripping sensitive headers.",
                                current_url,
                                next_url
                            );
                            let had_auth =
                                final_headers.contains_key(reqwest::header::AUTHORIZATION);
                            let had_cookie = final_headers.contains_key(reqwest::header::COOKIE);
                            final_headers.remove(reqwest::header::AUTHORIZATION);
                            final_headers.remove(reqwest::header::COOKIE);
                            if had_auth {
                                tracing::warn!(
                                    "Stripped Authorization header due to cross-origin redirect"
                                );
                            }
                            if had_cookie {
                                tracing::warn!(
                                    "Stripped Cookie header due to cross-origin redirect"
                                );
                            }
                        } else {
                            tracing::debug!(
                                "Redirecting same-origin from {} to {}. Preserving headers.",
                                current_url,
                                next_url
                            );
                        }

                        current_url = next_url;
                        attempts += 1;
                        // Redirects usually change to GET unless 307/308
                        if status != 307 && status != 308 {
                            tracing::debug!(
                                "Redirect {} changes method to GET, dropping body",
                                status
                            );
                            current_method = reqwest::Method::GET;
                            current_body_bytes = None;
                            current_multipart = None; // Body is dropped on redirect to GET
                        } else {
                            tracing::debug!("Redirect {} preserves method and body", status);
                        }
                continue;
            }

            // Final response
            let res_headers: HashMap<String, String> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            tracing::debug!("Response headers count: {}", res_headers.len());

            // Log authentication-related response headers
            if res_headers.contains_key("www-authenticate") {
                tracing::warn!(
                    "Response includes WWW-Authenticate header (auth challenge): {}",
                    res_headers
                        .get("www-authenticate")
                        .unwrap_or(&"unknown".to_string())
                );
            }
            if status == 401 {
                tracing::error!(
                    "Authentication failed: HTTP 401 Unauthorized from {}",
                    current_url
                );
            } else if status == 403 {
                tracing::error!(
                    "Authorization failed: HTTP 403 Forbidden from {}",
                    current_url
                );
            }

            let body_bytes = resp.bytes().await.map_err(|e| {
                tracing::error!("Failed to read response body: {}", e);
                e
            })?;
            let body_size = body_bytes.len();
            let body_text = String::from_utf8_lossy(&body_bytes).to_string();
            tracing::info!(
                "Response body size: {} bytes, is_utf8: {}",
                body_size,
                std::str::from_utf8(&body_bytes).is_ok()
            );

            // Try parse JSON
            let body_json: Option<Value> = serde_json::from_str(&body_text).ok();
            if body_json.is_some() {
                tracing::debug!("Response body is valid JSON");
            } else {
                tracing::debug!("Response body is not JSON");
            }

            // Build result
            #[derive(Serialize)]
            struct Output {
                status: u16,
                headers: HashMap<String, String>,
                body_text: String,
                body_json: Option<Value>,
            }

            let output = Output {
                status: status.as_u16(),
                headers: res_headers.clone(),
                body_text: body_text.clone(),
                body_json,
            };

            // Log complete response details
            self.log_response_details(status, &res_headers, &body_text);

            tracing::info!(
                "HTTP request completed successfully: {} {}",
                method,
                url_str
            );
            return Ok(serde_json::to_string_pretty(&output)?);
        }
    }
}
