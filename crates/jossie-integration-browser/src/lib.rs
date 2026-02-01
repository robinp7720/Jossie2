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
        selector: Option<&str>,
    ) -> anyhow::Result<String> {
        let url = Url::parse(url_str)?;
        let domain = url.domain().unwrap_or("unknown");

        tracing::info!("Browsing to: {}", url);

        // Hybrid approach: Check headers first.
        // If it's a simple resource (text, markdown, json, etc.), fetching it directly is faster and more reliable.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()?;

        // Try a HEAD request first to check content type
        let head_resp = client.head(url.clone()).send().await;

        let should_use_browser = match head_resp {
            Ok(resp) => {
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("")
                    .to_lowercase();

                tracing::info!("Content-Type for {}: {}", url, content_type);

                // If it's explicitly HTML, use browser to handle JS.
                // If it's something else (or unknown), but NOT html, try simple fetch.
                content_type.contains("html")
            }
            Err(_) => {
                // If HEAD fails (some servers block it), assume we might need browser
                // OR just try GET. Let's default to browser as it's the robust path for "web browsing".
                true
            }
        };

        if !should_use_browser {
            tracing::info!("Fetching {} with simple HTTP client", url);
            let resp = client.get(url.clone()).send().await?;
            if resp.status().is_success() {
                let text = resp.text().await?;
                return Ok(format!(
                    "### Content from {} (Direct Fetch)\n\n{}",
                    domain, text
                ));
            }
            // If simple fetch failed (e.g. 403 blocking non-browsers, though we set UA),
            // fall back to browser below.
            tracing::warn!(
                "Simple fetch failed with status {}, falling back to browser",
                resp.status()
            );
        }

        // Headless Chrome Path
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

        // Use selector if provided
        if let Some(sel) = selector {
            match tab.wait_for_element(sel) {
                Ok(_) => {}
                Err(e) => tracing::warn!("Selector {} not found: {}", sel, e),
            }
        }

        let content = tab
            .get_content()
            .map_err(|e| anyhow::anyhow!("Failed to get content: {}", e))?;

        // Convert to markdown
        let markdown = html2text::from_read(content.as_bytes(), 80);

        Ok(format!(
            "### Content from {} (Browser Rendered)\n\n{}",
            domain, markdown
        ))
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
