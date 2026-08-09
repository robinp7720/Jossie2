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
