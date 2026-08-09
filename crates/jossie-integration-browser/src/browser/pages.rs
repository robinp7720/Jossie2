impl BrowserIntegration {
    /// Launch or reuse the shared browser, then open a new tab.
    async fn browser_render(
        &self,
        url_str: &str,
        selector: Option<&str>,
    ) -> anyhow::Result<String> {
        let mut content = None;
        for attempt in 0..=1 {
            let url_owned = url_str.to_string();
            let selector_owned = selector.map(|s| s.to_string());
            let tab = self.open_browser_tab().await?;

            let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
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
            .map_err(|e| anyhow::anyhow!("Join error: {}", e))?;

            match result {
                Ok(html) => {
                    content = Some(html);
                    break;
                }
                Err(err)
                    if attempt == 0 && is_browser_connection_closed_message(&err.to_string()) =>
                {
                    self.invalidate_browser_state(&format!(
                        "shared browser connection closed while rendering '{}': {}",
                        url_str, err
                    ))
                    .await;
                }
                Err(err) => return Err(err),
            }
        }

        let content = content.ok_or_else(|| {
            anyhow::anyhow!("Failed to recover browser renderer after connection closure")
        })?;

        if is_bot_blocked(&content) {
            return Ok(
                "### Page blocked\nThe site blocked this request (bot detection / CAPTCHA). \
                 Try a different URL or approach."
                    .into(),
            );
        }

        let markdown = jossie_core::text::html_to_text(&content);

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

                if is_bot_blocked(&body) {
                    tracing::info!("Direct GET was bot-blocked, falling back to browser");
                } else {
                    // It's HTML — check if it looks like it needs JS rendering.
                    // Heuristics: very short body, or contains noscript warnings.
                    let needs_js =
                        body.len() < 1024 || (body.contains("<noscript>") && body.len() < 4096);

                    if needs_js {
                        tracing::info!(
                            "HTML response looks like it needs JS (len={}), falling back to browser",
                            body.len()
                        );
                    } else {
                        let markdown = jossie_core::text::html_to_text(&body);

                        if is_bot_blocked(&markdown) {
                            tracing::info!(
                                "Direct GET looked blocked after parsing, falling back to browser"
                            );
                        } else {
                            return Ok(format!(
                                "### Content from {} (Direct Fetch)\n\n{}",
                                domain, markdown
                            ));
                        }
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

}
