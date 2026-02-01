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
        tracing::info!("Initializing reqwest client for {}", url);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()?;

        // Try a HEAD request first to check content type
        tracing::info!("Sending HEAD request to {}", url);
        let head_resp = client
            .head(url.clone())
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
            )
            .send()
            .await;

        let mut should_use_browser = false;

        match head_resp {
            Ok(resp) => {
                let status = resp.status();
                tracing::info!("HEAD response status: {}", status);

                let headers = resp.headers();
                tracing::info!("HEAD response headers: {:?}", headers);

                let content_type = headers
                    .get("content-type")
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("")
                    .to_lowercase();

                tracing::info!("Content-Type for {}: '{}'", url, content_type);

                // If it's explicitly HTML, use browser to handle JS.
                if content_type.contains("html") {
                    tracing::info!("Detected HTML content, attempting browser fallback");
                    should_use_browser = true;
                } else {
                    tracing::info!("Content is not HTML, will attempt direct download");
                }
            }
            Err(e) => {
                tracing::warn!("HEAD request failed: {}. Defaulting to browser.", e);
                should_use_browser = true;
            }
        };

        if !should_use_browser {
            tracing::info!("Fetching {} with simple HTTP client (GET)", url);
            let resp_result = client
                .get(url.clone())
                .header(
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
                )
                .send()
                .await;

            match resp_result {
                Ok(resp) => {
                    let final_url = resp.url().clone();
                    let status = resp.status();
                    tracing::info!("GET final URL: {}", final_url);
                    tracing::info!("GET response status: {}", status);
                    tracing::info!("GET response headers: {:?}", resp.headers());

                    // Even if it is an error status (e.g. 400), we probably want to return the body
                    // so the agent can see "Rate Limit" or "Bad Request" details.
                    let body = resp
                        .text()
                        .await
                        .unwrap_or_else(|e| format!("Failed to read body: {}", e));

                    tracing::info!("GET response body length: {}", body.len());
                    if body.len() > 100 {
                        tracing::info!(
                            "GET response body preview: {}...",
                            &body[0..100].replace('\n', " ")
                        );
                    } else {
                        tracing::info!("GET response body: {}", body);
                    }

                    if !status.is_success() {
                        tracing::warn!("Simple fetch returned error status {}", status);
                        // Explicitly formatted error string for LLM awareness
                        return Ok(format!(
                            "### URL Fetch Failed\n**Final URL**: {}\n**Status**: {}\n**Reason**: Direct fetch returned error status.\n\n**Response Body**:\n```\n{}\n```",
                            final_url, status, body
                        ));
                    }

                    return Ok(format!(
                        "### Content from {} (Direct Fetch)\n\n{}",
                        domain, body
                    ));
                }
                Err(e) => {
                    tracing::warn!(
                        "Simple fetch network error: {}, falling back to browser request",
                        e
                    );
                    // Network failed, maybe browser can bypass unique network issues? Unlikely but worth a shot.
                }
            }
        }

        // Headless Chrome Path
        tracing::info!("Launching headless browser for {}", url);
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
        tracing::info!("Navigating browser tab to {}", url);
        tab.navigate_to(url_str)
            .map_err(|e| anyhow::anyhow!("Failed to navigate: {}", e))?;

        tracing::info!("Waiting for navigation to complete...");
        tab.wait_until_navigated()
            .map_err(|e| anyhow::anyhow!("Failed to wait for navigation: {}", e))?;

        // Use selector if provided
        if let Some(sel) = selector {
            tracing::info!("Waiting for selector: {}", sel);
            match tab.wait_for_element(sel) {
                Ok(_) => {}
                Err(e) => tracing::warn!("Selector {} not found: {}", sel, e),
            }
        }

        tracing::info!("Extracting page content");
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
