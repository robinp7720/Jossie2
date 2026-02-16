use headless_chrome::{Browser, LaunchOptions};
use jossie_core::integration::{Integration, ToolDefinition};
use std::sync::Arc;
use tokio::sync::OnceCell;

use url::Url;

/// Patterns that indicate a page blocked us (bot detection, CAPTCHA, etc.)
const BOT_BLOCK_PATTERNS: &[&str] = &[
    "unusual traffic",
    "please enable javascript",
    "captcha",
    "are not a robot",
    "blocked your ip",
    "access denied",
];

fn is_bot_blocked(content: &str) -> bool {
    let lower = content.to_lowercase();
    BOT_BLOCK_PATTERNS.iter().any(|p| lower.contains(p))
}

pub struct BrowserIntegration {
    client: reqwest::Client,
    browser: Arc<OnceCell<Browser>>,
}

impl BrowserIntegration {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .expect("Failed to build reqwest client");

        Self {
            client,
            browser: Arc::new(OnceCell::new()),
        }
    }

    /// Launch or reuse the shared browser, then open a new tab.
    async fn browser_render(&self, url_str: &str, selector: Option<&str>) -> anyhow::Result<String> {
        let browser = self
            .browser
            .get_or_try_init(|| async {
                tracing::info!("Launching shared headless browser instance");
                let options = LaunchOptions::default_builder()
                    .headless(true)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build launch options: {}", e))?;

                let b = tokio::task::spawn_blocking(move || Browser::new(options))
                    .await
                    .map_err(|e| anyhow::anyhow!("Join error launching browser: {}", e))?
                    .map_err(|e| anyhow::anyhow!("Failed to launch browser: {}", e))?;

                Ok::<Browser, anyhow::Error>(b)
            })
            .await?;

        let url_owned = url_str.to_string();
        let selector_owned = selector.map(|s| s.to_string());

        // headless_chrome is sync — open a tab here, then run navigation on a blocking thread.
        let tab = browser
            .new_tab()
            .map_err(|e| anyhow::anyhow!("Failed to open tab: {}", e))?;

        let content = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            tracing::info!("Navigating browser tab to {}", url_owned);
            tab.navigate_to(&url_owned)
                .map_err(|e| anyhow::anyhow!("Failed to navigate: {}", e))?;

            tab.wait_until_navigated()
                .map_err(|e| anyhow::anyhow!("Failed to wait for navigation: {}", e))?;

            if let Some(sel) = &selector_owned {
                tracing::info!("Waiting for selector: {}", sel);
                if let Err(e) = tab.wait_for_element(sel) {
                    tracing::warn!("Selector {} not found: {}", sel, e);
                }
            }

            let html = tab
                .get_content()
                .map_err(|e| anyhow::anyhow!("Failed to get content: {}", e))?;

            Ok(html)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Join error: {}", e))??;

        let markdown = html2text::from_read(content.as_bytes(), 80);

        if is_bot_blocked(&markdown) {
            return Ok(
                "### Page blocked\nThe site blocked this request (bot detection / CAPTCHA). \
                 Try a different URL or approach."
                    .into(),
            );
        }

        Ok(markdown)
    }

    async fn browser_read_page(
        &self,
        url_str: &str,
        selector: Option<&str>,
    ) -> anyhow::Result<String> {
        let url = Url::parse(url_str)?;
        let domain = url.domain().unwrap_or("unknown");

        tracing::info!("Browsing to: {}", url);

        // GET-first approach: try a direct GET and only fall back to the browser
        // if the content looks like it needs JS rendering.
        tracing::info!("Fetching {} with direct GET", url);
        let resp_result = self
            .client
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
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("")
                    .to_lowercase();

                tracing::info!(
                    "GET {} -> status={}, content-type='{}'",
                    final_url,
                    status,
                    content_type
                );

                if !status.is_success() {
                    let body = resp
                        .text()
                        .await
                        .unwrap_or_else(|e| format!("Failed to read body: {}", e));
                    return Ok(format!(
                        "### URL Fetch Failed\n**Final URL**: {}\n**Status**: {}\n\n```\n{}\n```",
                        final_url, status, body
                    ));
                }

                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|e| format!("Failed to read body: {}", e));

                // If it's not HTML, return directly (JSON, plain text, etc.)
                if !content_type.contains("html") {
                    return Ok(format!(
                        "### Content from {} (Direct Fetch)\n\n{}",
                        domain, body
                    ));
                }

                // It's HTML — check if it looks like it needs JS rendering.
                // Heuristics: very short body, or contains noscript warnings.
                let needs_js = body.len() < 1024
                    || (body.contains("<noscript>") && body.len() < 4096);

                if needs_js {
                    tracing::info!(
                        "HTML response looks like it needs JS (len={}), falling back to browser",
                        body.len()
                    );
                } else {
                    let markdown = html2text::from_read(body.as_bytes(), 80);

                    if is_bot_blocked(&markdown) {
                        tracing::info!("Direct GET was bot-blocked, falling back to browser");
                    } else {
                        return Ok(format!(
                            "### Content from {} (Direct Fetch)\n\n{}",
                            domain, markdown
                        ));
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Direct GET failed: {}, falling back to browser", e);
            }
        }

        // Headless Chrome fallback
        tracing::info!("Using headless browser for {}", url);
        let markdown = self.browser_render(url_str, selector).await?;

        Ok(format!(
            "### Content from {} (Browser Rendered)\n\n{}",
            domain, markdown
        ))
    }

    async fn browser_search(&self, query: &str) -> anyhow::Result<String> {
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );

        tracing::info!("Searching DuckDuckGo HTML for: {}", query);

        let resp = self
            .client
            .get(&url)
            .header("Accept", "text/html")
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Ok(format!(
                "### Search Failed\nDuckDuckGo returned status {}. Try again later.",
                status
            ));
        }

        let body = resp.text().await?;
        let markdown = html2text::from_read(body.as_bytes(), 80);

        if is_bot_blocked(&markdown) {
            return Ok(
                "### Search blocked\nThe search engine blocked this request (bot detection). \
                 Try a different query or wait."
                    .into(),
            );
        }

        Ok(format!("### Search Results\n\n{}", markdown))
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
                description: "Searches the web for a query using DuckDuckGo.".to_string(),
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
