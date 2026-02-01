use base64::Engine;
use jossie_core::integration::{Integration, ToolDefinition};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use url::Url;

// Ensure we don't allow SSRF to internal networks
fn is_globally_reachable(url: &Url) -> bool {
    // Simple check: scheme must be http or https
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }

    #[cfg(test)]
    if url.host_str() == Some("127.0.0.1") || url.host_str() == Some("localhost") {
        return true;
    }

    // Check host type
    match url.host_str() {
        Some(host) => {
            // Block localhost, 127.0.0.1, internal ranges, etc.
            // This is a basic implementation. For robust SSRF, we'd need to resolve IP and check against private ranges.
            // For this iterate, we'll block common local identifiers.
            let lower = host.to_lowercase();
            if lower == "localhost"
                || lower.starts_with("127.")
                || lower.starts_with("10.")
                || lower.starts_with("192.168.")
                || lower == "::1"
            {
                return false;
            }
            true
        }
        None => false,
    }
}

pub struct HttpIntegration {
    allowed_domains: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct MultipartField {
    name: String,
    value: String,
}

#[derive(Deserialize, Debug)]
struct MultipartFile {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    data_base64: String,
}

#[derive(Deserialize, Debug)]
struct MultipartBody {
    #[serde(rename = "type")]
    body_type: String, // must be "multipart"
    fields: Option<Vec<MultipartField>>,
    files: Option<Vec<MultipartFile>>,
}

enum BodyContent {
    None,
    Text(String),
    Json(Vec<u8>), // already serialized bytes
    Multipart(reqwest::multipart::Form),
}

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
        let url = Url::parse(url_str).map_err(|e| anyhow::anyhow!("Invalid URL: {}", e))?;

        if !is_globally_reachable(&url) {
            return Err(anyhow::anyhow!(
                "Blocked: URL targets a local or private network address."
            ));
        }

        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| anyhow::anyhow!("Invalid HTTP method"))?;

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
                }
            }
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
            BodyContent::None => {}
            BodyContent::Text(s) => {
                current_body_bytes = Some(s.into_bytes());
            }
            BodyContent::Json(bytes) => {
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
            if final_headers.contains_key(reqwest::header::AUTHORIZATION) {
                if let Some(host) = current_url.host_str() {
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
                        return Err(anyhow::anyhow!(
                            "Authentication header present but domain '{}' is not in allowed_domains list.",
                            host
                        ));
                    }
                }
            }

            tracing::info!("HTTP {} {}", current_method, current_url);
            tracing::debug!("Headers: {}", self.redact_headers(&final_headers));

            let mut req_builder = client
                .request(current_method.clone(), current_url.clone())
                .headers(final_headers.clone());

            if let Some(bytes) = &current_body_bytes {
                // GET usually doesn't have body, but we allow it if user provided it
                req_builder = req_builder.body(bytes.clone());
            } else if let Some(form) = current_multipart.take() {
                req_builder = req_builder.multipart(form);
            }

            let resp = req_builder.send().await?;
            let status = resp.status();

            // Check if redirect
            if status.is_redirection() && follow_redirects && attempts < max_attempts {
                if let Some(location) = resp.headers().get("Location") {
                    if let Ok(loc_str) = location.to_str() {
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
                            final_headers.remove(reqwest::header::AUTHORIZATION);
                            final_headers.remove(reqwest::header::COOKIE);
                        }

                        current_url = next_url;
                        attempts += 1;
                        // Redirects usually change to GET unless 307/308
                        if status != 307 && status != 308 {
                            current_method = reqwest::Method::GET;
                            current_body_bytes = None;
                            current_multipart = None; // Body is dropped on redirect to GET
                        }
                        continue;
                    }
                }
            }

            // Final response
            let res_headers: HashMap<String, String> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            let body_bytes = resp.bytes().await?;
            let body_text = String::from_utf8_lossy(&body_bytes).to_string();

            // Try parse JSON
            let body_json: Option<Value> = serde_json::from_str(&body_text).ok();

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
                headers: res_headers,
                body_text,
                body_json,
            };

            return Ok(serde_json::to_string_pretty(&output)?);
        }
    }
}

#[async_trait::async_trait]
impl Integration for HttpIntegration {
    fn name(&self) -> &str {
        "http"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "http_request".to_string(),
            description: "Makes an HTTP request with custom method, headers, and body. Supports JSON automatically.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"], "description": "HTTP Method" },
                    "url": { "type": "string", "description": "Target URL (must be absolute)" },
                    "headers": { "type": ["string", "null"], "description": "Request headers as a JSON string (e.g. '{\"Content-Type\": \"application/json\"}')." },
                    "query": { "type": ["string", "null"], "description": "Query parameters as a JSON string (e.g. '{\"q\": \"search\"}')." },
                    "body": { "type": ["string", "null"], "description": "Request body as a JSON string. For plain text, pass directly. For JSON data, stringify the object. For multipart/form-data, stringify an object with structure: {\"type\": \"multipart\", \"fields\": [{\"name\": \"...\", \"value\": \"...\"}], \"files\": [{\"name\": \"...\", \"filename\": \"...\", \"content_type\": \"...\", \"data_base64\": \"...\"}]}." },
                    "timeout_ms": { "type": ["number", "null"], "description": "Timeout in milliseconds (default 20000)" },
                    "follow_redirects": { "type": ["boolean", "null"], "description": "Whether to follow redirects (default false)" }
                },
                "required": ["method", "url", "headers", "query", "body", "timeout_ms", "follow_redirects"],
                "additionalProperties": false
            }),
        }]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        if tool_name != "http_request" {
            anyhow::bail!("Unknown tool: {}", tool_name);
        }

        let args: Value = serde_json::from_str(arguments)?;

        // Extract args manually to handle types
        let method = args["method"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'method'"))?;
        let url = args["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'url'"))?;

        let headers = match args.get("headers") {
            Some(Value::String(s)) => {
                let m: HashMap<String, String> = serde_json::from_str(s)
                    .map_err(|e| anyhow::anyhow!("Failed to parse headers JSON string: {}", e))?;
                Some(m)
            }
            _ => None,
        };

        let query = match args.get("query") {
            Some(Value::String(s)) => {
                let m: HashMap<String, Value> = serde_json::from_str(s)
                    .map_err(|e| anyhow::anyhow!("Failed to parse query JSON string: {}", e))?;
                Some(m)
            }
            _ => None,
        };

        // Body Parsing
        let raw_body_val = args.get("body");
        let body_content = if let Some(val) = raw_body_val {
            match val {
                Value::Null => BodyContent::None,
                Value::String(s) => {
                    // Try to parse as JSON to detect multipart structure
                    if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                        if let Value::Object(map) = &parsed {
                            if map.get("type").and_then(|v| v.as_str()) == Some("multipart") {
                                // It's a multipart request - parse and build form
                                let mp_body: MultipartBody = serde_json::from_value(parsed)
                                    .map_err(|e| {
                                        anyhow::anyhow!("Invalid multipart body structure: {}", e)
                                    })?;

                                let mut form = reqwest::multipart::Form::new();

                                if let Some(fields) = mp_body.fields {
                                    for field in fields {
                                        form = form.text(field.name, field.value);
                                    }
                                }

                                if let Some(files) = mp_body.files {
                                    for file in files {
                                        let engine = base64::engine::general_purpose::STANDARD;
                                        let bytes =
                                            engine.decode(&file.data_base64).map_err(|e| {
                                                anyhow::anyhow!(
                                                    "Invalid base64 in file '{}': {}",
                                                    file.name,
                                                    e
                                                )
                                            })?;

                                        if bytes.len() > 500 * 1024 {
                                            anyhow::bail!(
                                                "File '{}' exceeds 500KB limit",
                                                file.name
                                            );
                                        }

                                        let mut part = reqwest::multipart::Part::bytes(bytes);
                                        if let Some(fnm) = file.filename {
                                            part = part.file_name(fnm);
                                        }
                                        if let Some(ct) = file.content_type {
                                            part = part.mime_str(&ct).map_err(|e| {
                                                anyhow::anyhow!("Invalid mime type: {}", e)
                                            })?;
                                        }

                                        form = form.part(file.name, part);
                                    }
                                }

                                BodyContent::Multipart(form)
                            } else {
                                // JSON object but not multipart
                                BodyContent::Json(serde_json::to_vec(&parsed)?)
                            }
                        } else {
                            // Not an object, treat as text
                            BodyContent::Text(s.clone())
                        }
                    } else {
                        // Not valid JSON, treat as plain text
                        BodyContent::Text(s.clone())
                    }
                }
                // Fallback for JSON object/array (legacy, shouldn't happen with new schema)
                obj @ Value::Object(_) | obj @ Value::Array(_) => {
                    let bytes = serde_json::to_vec(&obj)?;
                    BodyContent::Json(bytes)
                }
                _ => BodyContent::None,
            }
        } else {
            BodyContent::None
        };

        // Handle number/null for timeout
        let timeout_ms = match args.get("timeout_ms") {
            Some(Value::Number(n)) => n.as_u64(),
            _ => None,
        };

        // Handle boolean/null for follow_redirects
        let follow_redirects = match args.get("follow_redirects") {
            Some(Value::Bool(b)) => *b,
            _ => false,
        };

        self.http_request(
            method,
            url,
            headers,
            query,
            body_content,
            timeout_ms,
            follow_redirects,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_globally_reachable() {
        assert!(is_globally_reachable(
            &Url::parse("https://google.com").unwrap()
        ));
        // In test mode, we allow localhost/127.0.0.1 for integration tests
        assert!(is_globally_reachable(
            &Url::parse("http://localhost:8080").unwrap()
        ));
        assert!(is_globally_reachable(
            &Url::parse("http://127.0.0.1").unwrap()
        ));
        // Internal IPs like 10.x are still blocked if they don't match localhost check logic (which only checks hostname)
        // is_globally_reachable check for "10." is: lower.starts_with("10.")
        // Our test override only checks for "localhost" or "127.0.0.1".
        // So 10.0.0.5 should still fail.
        assert!(!is_globally_reachable(
            &Url::parse("http://10.0.0.5").unwrap()
        ));
    }

    #[test]
    fn test_redact_headers() {
        let integration = HttpIntegration::new(vec![]);
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", HeaderValue::from_static("Bearer secret"));
        headers.insert("X-Api-Key", HeaderValue::from_static("12345"));
        headers.insert("Cookie", HeaderValue::from_static("session=abc"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));

        let redacted = integration.redact_headers(&headers);
        assert!(redacted.contains("authorization"));
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("Bearer secret"));

        assert!(redacted.contains("x-api-key"));
        assert!(!redacted.contains("12345"));

        assert!(redacted.contains("cookie"));
        assert!(!redacted.contains("session=abc"));
        assert!(redacted.contains("content-type"));
        assert!(redacted.contains("application/json"));
    }

    // --- Multipart Integration Tests ---

    use axum::Router;
    use axum::extract::Multipart;
    use axum::routing::post;
    use base64::Engine;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_multipart_upload() {
        // Start a local axum server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let app = Router::new().route("/upload", post(|mut multipart: Multipart| async move {
            let mut fields: HashMap<String, String> = HashMap::new();
            let mut files: Vec<(String, String, Option<String>, usize)> = Vec::new();

            while let Some(field) = multipart.next_field().await.unwrap() {
                let name = field.name().unwrap().to_string();
                if let Some(filename) = field.file_name() {
                    let filename = filename.to_string();
                    let content_type = field.content_type().map(|s| s.to_string());
                    let data = field.bytes().await.unwrap();
                    files.push((name, filename, content_type, data.len()));
                } else {
                    let data = field.text().await.unwrap();
                    fields.insert(name, data);
                }
            }

            // Return validation result as JSON
            serde_json::json!({
                "fields": fields,
                "files_count": files.len(),
                "file_info": files.into_iter().map(|(n, fnm, ct, len)| (n, fnm, ct, len)).collect::<Vec<_>>()
            }).to_string()
        }));

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Construct client and make request
        let integration = HttpIntegration::new(vec![]);

        // Prepare base64 image
        let dummy_data = b"Hello World Image";
        let engine = base64::engine::general_purpose::STANDARD;
        let b64_data = engine.encode(dummy_data);

        let body_json = serde_json::json!({
            "type": "multipart",
            "fields": [
                {"name": "field1", "value": "value1"},
                {"name": "mode", "value": "test"}
            ],
            "files": [
                {
                    "name": "upfile",
                    "filename": "test.png",
                    "content_type": "image/png",
                    "data_base64": b64_data
                }
            ]
        });

        let args = serde_json::json!({
            "method": "POST",
            "url": format!("http://{}/upload", addr),
            "headers": null,
            "query": null,
            "body": body_json.to_string(),  // Stringify the body
            "timeout_ms": 5000,
            "follow_redirects": false
        });

        let result = integration
            .execute("http_request", &args.to_string())
            .await
            .expect("Request failed");

        let output: Value = serde_json::from_str(&result).unwrap();
        let resp_body: Value = serde_json::from_str(output["body_text"].as_str().unwrap()).unwrap();

        assert_eq!(resp_body["fields"]["field1"], "value1");
        assert_eq!(resp_body["fields"]["mode"], "test");
        assert_eq!(resp_body["files_count"], 1);

        let file_info = resp_body["file_info"].as_array().unwrap()[0]
            .as_array()
            .unwrap();
        assert_eq!(file_info[0], "upfile");
        assert_eq!(file_info[1], "test.png");
        assert_eq!(file_info[2], "image/png");
        assert_eq!(file_info[3], dummy_data.len());
    }

    #[tokio::test]
    async fn test_multipart_size_limit() {
        let integration = HttpIntegration::new(vec![]);

        // Create 600KB dummy data
        let large_data = vec![0u8; 600 * 1024];
        let engine = base64::engine::general_purpose::STANDARD;
        let b64_data = engine.encode(large_data);

        let body_json = serde_json::json!({
            "type": "multipart",
            "fields": [],
            "files": [
                {
                    "name": "too_big",
                    "filename": "big.bin",
                    "data_base64": b64_data
                }
            ]
        });

        let args = serde_json::json!({
            "method": "POST",
            "url": "http://127.0.0.1:1234/upload",
            "headers": null,
            "query": null,
            "body": body_json.to_string(),  // Stringify the body
            "timeout_ms": 1000,
            "follow_redirects": false
        });

        let result = integration.execute("http_request", &args.to_string()).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("exceeds 500KB limit")
        );
    }
}
