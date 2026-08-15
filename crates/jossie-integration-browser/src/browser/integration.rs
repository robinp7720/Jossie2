#[async_trait::async_trait]
impl Integration for BrowserIntegration {
    fn name(&self) -> &str {
        "browser"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::for_args::<BrowserReadPageArgs>("browser_read_page", "Visits a web page and returns its content as markdown. Handles JS-heavy sites."),
            ToolDefinition::for_args::<BrowserOpenSessionArgs>("browser_open_session", "Open an interactive browser session for a site that needs logins, clicks, forms, or preserved cookies. Returns a session snapshot with visible inputs, selects, and actions."),
            ToolDefinition::for_args::<BrowserSessionSnapshotArgs>("browser_session_snapshot", "Return the current state of an interactive browser session, including page text preview and visible interactive elements."),
            ToolDefinition::for_args::<BrowserNavigateArgs>("browser_navigate", "Navigate an existing interactive browser session to a new URL and keep the same cookies and login state."),
            ToolDefinition::for_args::<BrowserFillInputArgs>("browser_fill_input", "Fill a visible input or textarea in an interactive browser session. Prefer `selector` when known, otherwise use `id`, `name`, `label`, or `placeholder`."),
            ToolDefinition::for_args::<BrowserClickArgs>("browser_click", "Click a visible link, button, or submit control in an interactive browser session. Use `selector` when available, otherwise use a visible `text` snippet."),
            ToolDefinition::for_args::<BrowserSelectOptionArgs>("browser_select_option", "Choose an option in a visible `<select>` element in an interactive browser session."),
            ToolDefinition::for_args::<BrowserCloseSessionArgs>("browser_close_session", "Close an interactive browser session and discard its cookies and page state."),
            ToolDefinition::for_args::<BrowserSearchArgs>("browser_search", "Searches the web for a query using multiple search providers."),
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        let args: serde_json::Value = serde_json::from_str(arguments)?;

        match tool_name {
            "browser_read_page" => {
                let args: BrowserReadPageArgs = serde_json::from_value(args)?;
                self.browser_read_page(&args.url, args.selector.as_deref()).await
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
                let args: BrowserSearchArgs = serde_json::from_value(args)?;
                self.browser_search(&args.query).await
            }
            _ => anyhow::bail!("Unknown tool: {}", tool_name),
        }
    }
}
