#[async_trait::async_trait]
impl Integration for BrowserIntegration {
    fn name(&self) -> &str {
        "browser"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "browser_read_page".to_string(),
                description:
                    "Visits a web page and returns its content as markdown. Handles JS-heavy sites."
                        .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The URL to visit"
                        },
                        "selector": {
                            "type": ["string", "null"],
                            "description": "Optional CSS selector to focus on. If omitted, captures whole body. Pass null if not used."
                        }
                    },
                    "required": ["url", "selector"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "browser_open_session".to_string(),
                description: "Open an interactive browser session for a site that needs logins, clicks, forms, or preserved cookies. Returns a session snapshot with visible inputs, selects, and actions."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The URL to open in a persistent browser tab"
                        },
                        "wait_for": {
                            "type": ["string", "null"],
                            "description": "Optional CSS selector to wait for after navigation"
                        }
                    },
                    "required": ["url"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "browser_session_snapshot".to_string(),
                description: "Return the current state of an interactive browser session, including page text preview and visible interactive elements."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "The browser session to inspect"
                        }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "browser_navigate".to_string(),
                description: "Navigate an existing interactive browser session to a new URL and keep the same cookies and login state."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "The browser session to reuse"
                        },
                        "url": {
                            "type": "string",
                            "description": "The destination URL"
                        },
                        "wait_for": {
                            "type": ["string", "null"],
                            "description": "Optional CSS selector to wait for after navigation"
                        }
                    },
                    "required": ["session_id", "url"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "browser_fill_input".to_string(),
                description: "Fill a visible input or textarea in an interactive browser session. Prefer `selector` when known, otherwise use `id`, `name`, `label`, or `placeholder`."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": {"type": "string", "description": "The browser session to use"},
                        "selector": {"type": ["string", "null"], "description": "Optional CSS selector for the input"},
                        "id": {"type": ["string", "null"], "description": "Optional element id"},
                        "name": {"type": ["string", "null"], "description": "Optional input name"},
                        "label": {"type": ["string", "null"], "description": "Optional visible label text"},
                        "placeholder": {"type": ["string", "null"], "description": "Optional placeholder text"},
                        "value": {"type": "string", "description": "The value to enter"}
                    },
                    "required": ["session_id", "value"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "browser_click".to_string(),
                description: "Click a visible link, button, or submit control in an interactive browser session. Use `selector` when available, otherwise use a visible `text` snippet."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": {"type": "string", "description": "The browser session to use"},
                        "selector": {"type": ["string", "null"], "description": "Optional CSS selector for the clickable element"},
                        "text": {"type": ["string", "null"], "description": "Optional visible text to match on links or buttons"},
                        "tag": {"type": ["string", "null"], "description": "Optional tag name filter such as `a` or `button`"},
                        "wait_after_ms": {"type": "integer", "description": "How long to wait after the click before capturing the next snapshot. Defaults to 1200."}
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "browser_select_option".to_string(),
                description: "Choose an option in a visible `<select>` element in an interactive browser session."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": {"type": "string", "description": "The browser session to use"},
                        "selector": {"type": ["string", "null"], "description": "Optional CSS selector for the select element"},
                        "id": {"type": ["string", "null"], "description": "Optional element id"},
                        "name": {"type": ["string", "null"], "description": "Optional select name"},
                        "label": {"type": ["string", "null"], "description": "Optional visible label text"},
                        "text": {"type": ["string", "null"], "description": "Optional visible option text to select"},
                        "value": {"type": ["string", "null"], "description": "Optional option value to select"},
                        "wait_after_ms": {"type": "integer", "description": "How long to wait after the selection before capturing the next snapshot. Defaults to 1200."}
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "browser_close_session".to_string(),
                description: "Close an interactive browser session and discard its cookies and page state."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "The browser session to close"
                        }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "browser_search".to_string(),
                description: "Searches the web for a query using multiple search providers."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query"
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        let args: serde_json::Value = serde_json::from_str(arguments)?;

        match tool_name {
            "browser_read_page" => {
                let url = args["url"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing url"))?;
                let selector = args.get("selector").and_then(|v| v.as_str());
                self.browser_read_page(url, selector).await
            }
            "browser_open_session" => {
                let args: BrowserOpenSessionArgs = serde_json::from_value(args)?;
                self.browser_open_session(&args.url, args.wait_for.as_deref())
                    .await
            }
            "browser_session_snapshot" => {
                let args: BrowserSessionSnapshotArgs = serde_json::from_value(args)?;
                self.browser_session_snapshot(&args.session_id).await
            }
            "browser_navigate" => {
                let args: BrowserNavigateArgs = serde_json::from_value(args)?;
                self.browser_navigate(&args.session_id, &args.url, args.wait_for.as_deref())
                    .await
            }
            "browser_fill_input" => {
                let args: BrowserFillInputArgs = serde_json::from_value(args)?;
                self.browser_fill_input(&args).await
            }
            "browser_click" => {
                let args: BrowserClickArgs = serde_json::from_value(args)?;
                if args.selector.is_none() && args.text.is_none() {
                    anyhow::bail!("browser_click requires either selector or text");
                }
                self.browser_click(&args).await
            }
            "browser_select_option" => {
                let args: BrowserSelectOptionArgs = serde_json::from_value(args)?;
                if args.selector.is_none()
                    && args.id.is_none()
                    && args.name.is_none()
                    && args.label.is_none()
                {
                    anyhow::bail!("browser_select_option requires selector, id, name, or label");
                }
                if args.text.is_none() && args.value.is_none() {
                    anyhow::bail!("browser_select_option requires text or value");
                }
                self.browser_select_option(&args).await
            }
            "browser_close_session" => {
                let args: BrowserCloseSessionArgs = serde_json::from_value(args)?;
                self.browser_close_session(&args.session_id).await
            }
            "browser_search" => {
                let query = args["query"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing query"))?;
                self.browser_search(query).await
            }
            _ => anyhow::bail!("Unknown tool: {}", tool_name),
        }
    }
}
