use headless_chrome::{Browser, LaunchOptions};
use jossie_core::integration::{Integration, ToolDefinition};

use url::Url;

pub struct BrowserIntegration;

impl BrowserIntegration {
    pub fn new() -> Self {
        Self
    }

    async fn browser_read_page(
        &self,
        url_str: &str,
        _selector: Option<&str>,
    ) -> anyhow::Result<String> {
        let url = Url::parse(url_str)?;
        let domain = url.domain().unwrap_or("unknown");

        tracing::info!("Browsing to: {}", url);

        // Configure browser launch
        let options = LaunchOptions::default_builder()
            .headless(true)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed into build launch options: {}", e))?;

        let browser = Browser::new(options)
            .map_err(|e| anyhow::anyhow!("Failed to launch browser: {}", e))?;

        let tab = browser
            .new_tab()
            .map_err(|e| anyhow::anyhow!("Failed to open tab: {}", e))?;

        // Navigate
        tab.navigate_to(url_str)
            .map_err(|e| anyhow::anyhow!("Failed to navigate: {}", e))?;

        tab.wait_until_navigated()
            .map_err(|e| anyhow::anyhow!("Failed to wait for navigation: {}", e))?;

        // Wait a bit for dynamic content? Alternatively wait for selector if provided.
        // For now, let's just grab the content after load.

        let content = tab
            .get_content()
            .map_err(|e| anyhow::anyhow!("Failed to get content: {}", e))?;

        // Convert to markdown
        // html2text::from_read(content.as_bytes(), 80) is synchronous/blocking, but it's fast enough for text.
        let markdown = html2text::from_read(content.as_bytes(), 80);

        Ok(format!("### Content from {}\n\n{}", domain, markdown))
    }

    async fn browser_search(&self, query: &str) -> anyhow::Result<String> {
        // Use DuckDuckGo for privacy and simpler HTML structure if we were scraping,
        // or Google. Let's try Google first.
        let url = format!(
            "https://www.google.com/search?q={}",
            urlencoding::encode(query)
        );
        self.browser_read_page(&url, None).await
    }
}

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
                name: "browser_search".to_string(),
                description: "Searches the web for a query using a search engine.".to_string(),
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
