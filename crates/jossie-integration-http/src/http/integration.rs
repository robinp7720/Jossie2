#[async_trait::async_trait]
impl Integration for HttpIntegration {
    fn name(&self) -> &str {
        "http"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::for_args::<HttpRequestArgs>(
            "http_request",
            "Makes an HTTP request with custom method, headers, and body. Supports JSON automatically.",
        )]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        if tool_name != "http_request" {
            anyhow::bail!("Unknown tool: {}", tool_name);
        }

        let args: HttpRequestArgs = serde_json::from_str(arguments)?;
        let method = args.method.as_str();
        let url = args.url.as_str();

        let headers = match args.headers.as_deref() {
            Some(s) => {
                let m: HashMap<String, String> = serde_json::from_str(s)
                    .map_err(|e| anyhow::anyhow!("Failed to parse headers JSON string: {}", e))?;
                Some(m)
            }
            _ => None,
        };

        let query = match args.query.as_deref() {
            Some(s) => {
                let m: HashMap<String, Value> = serde_json::from_str(s)
                    .map_err(|e| anyhow::anyhow!("Failed to parse query JSON string: {}", e))?;
                Some(m)
            }
            _ => None,
        };

        // Body Parsing
        let body_content = if let Some(s) = args.body.as_deref() {
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
                            BodyContent::Text(s.to_string())
                        }
                    } else {
                        // Not valid JSON, treat as plain text
                        BodyContent::Text(s.to_string())
                    }
        } else {
            BodyContent::None
        };

        // Handle number/null for timeout
        let timeout_ms = args.timeout_ms;

        // Handle boolean/null for follow_redirects
        let follow_redirects = args.follow_redirects.unwrap_or(false);

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
