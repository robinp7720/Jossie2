impl BrowserIntegration {
    async fn fetch_search_html(
        &self,
        provider: SearchProvider,
        url: &str,
    ) -> anyhow::Result<String> {
        let resp = self
            .client
            .get(url)
            .header("Accept", "text/html,application/xhtml+xml")
            .header("Accept-Language", "en-US,en;q=0.9")
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;

        if !status.is_success() {
            anyhow::bail!("{} returned status {}", provider.label(), status);
        }

        if is_bot_blocked(&body) || is_bot_blocked(&jossie_core::text::html_to_text(&body)) {
            anyhow::bail!("{} returned a bot challenge", provider.label());
        }

        Ok(body)
    }

    async fn search_duckduckgo_lite(&self, query: &str) -> anyhow::Result<Vec<SearchResult>> {
        let url = format!(
            "https://lite.duckduckgo.com/lite/?q={}",
            urlencoding::encode(query)
        );
        tracing::info!("Searching DuckDuckGo Lite for: {}", query);

        let body = self
            .fetch_search_html(SearchProvider::DuckDuckGoLite, &url)
            .await?;
        let results = parse_duckduckgo_lite_results(&body);

        if results.is_empty() {
            anyhow::bail!("DuckDuckGo Lite returned no parseable results");
        }

        Ok(results)
    }

    async fn search_brave_html(&self, query: &str) -> anyhow::Result<Vec<SearchResult>> {
        let url = format!(
            "https://search.brave.com/search?q={}&source=web",
            urlencoding::encode(query)
        );
        tracing::info!("Searching Brave for: {}", query);

        let body = self
            .fetch_search_html(SearchProvider::BraveHtml, &url)
            .await?;
        let results = parse_brave_results(&body);

        if results.is_empty() {
            anyhow::bail!("Brave returned no parseable results");
        }

        Ok(results)
    }

    async fn search_duckduckgo_instant_answer(
        &self,
        query: &str,
    ) -> anyhow::Result<Option<String>> {
        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_redirect=1&no_html=1",
            urlencoding::encode(query)
        );
        tracing::info!("Searching DuckDuckGo Instant Answer for: {}", query);

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!(
                "{} returned status {}",
                SearchProvider::DuckDuckGoInstantAnswer.label(),
                resp.status()
            );
        }

        let payload = resp.json::<serde_json::Value>().await?;
        let heading = payload
            .get("Heading")
            .and_then(|value| value.as_str())
            .map(collapse_whitespace)
            .unwrap_or_default();
        let abstract_text = payload
            .get("AbstractText")
            .and_then(|value| value.as_str())
            .map(collapse_whitespace)
            .unwrap_or_default();
        let abstract_url = payload
            .get("AbstractURL")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();

        let mut related = payload
            .get("Results")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let text = item.get("Text").and_then(|value| value.as_str())?;
                        let url = item.get("FirstURL").and_then(|value| value.as_str())?;
                        SearchResult::new(text.to_string(), url.to_string(), String::new())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if let Some(topics) = payload
            .get("RelatedTopics")
            .and_then(|value| value.as_array())
        {
            collect_instant_answer_topics(topics, &mut related);
        }

        if heading.is_empty() && abstract_text.is_empty() && related.is_empty() {
            return Ok(None);
        }

        let mut out = vec![
            "### Search Results".to_string(),
            format!(
                "Provider: {}",
                SearchProvider::DuckDuckGoInstantAnswer.label()
            ),
            format!("Query: {query}"),
            String::new(),
        ];

        if !heading.is_empty() || !abstract_text.is_empty() {
            let mut summary = heading;
            if !abstract_text.is_empty() {
                if !summary.is_empty() {
                    summary.push_str(": ");
                }
                summary.push_str(&abstract_text);
            }
            out.push(format!("Summary: {summary}"));
            if !abstract_url.is_empty() {
                out.push(format!("Source: {abstract_url}"));
            }
            out.push(String::new());
        }

        for (index, result) in related.iter().take(SEARCH_RESULT_LIMIT).enumerate() {
            out.push(format!("{}. {}", index + 1, result.title));
            out.push(format!("URL: {}", result.url));
            if !result.snippet.is_empty() {
                out.push(format!("Snippet: {}", result.snippet));
            }
            out.push(String::new());
        }

        Ok(Some(out.join("\n").trim().to_string()))
    }

    async fn browser_search(&self, query: &str) -> anyhow::Result<String> {
        let mut failures = Vec::new();

        for provider in [SearchProvider::DuckDuckGoLite, SearchProvider::BraveHtml] {
            let attempt = match provider {
                SearchProvider::DuckDuckGoLite => self.search_duckduckgo_lite(query).await,
                SearchProvider::BraveHtml => self.search_brave_html(query).await,
                SearchProvider::DuckDuckGoInstantAnswer => unreachable!(),
            };

            match attempt {
                Ok(results) => return Ok(format_search_results(query, provider, &results)),
                Err(err) => {
                    tracing::warn!("{} search failed: {}", provider.label(), err);
                    failures.push(format!("{}: {}", provider.label(), err));
                }
            }
        }

        match self.search_duckduckgo_instant_answer(query).await {
            Ok(Some(summary)) => return Ok(summary),
            Ok(None) => failures.push(format!(
                "{}: no summary or related topics returned",
                SearchProvider::DuckDuckGoInstantAnswer.label()
            )),
            Err(err) => failures.push(format!(
                "{}: {}",
                SearchProvider::DuckDuckGoInstantAnswer.label(),
                err
            )),
        }

        Ok(format_search_failures(query, &failures))
    }
}
