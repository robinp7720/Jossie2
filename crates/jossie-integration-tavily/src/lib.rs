use jossie_core::integration::{Integration, ToolDefinition};
use serde::Deserialize;

pub struct TavilyIntegration {
    api_key: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    #[serde(default)]
    content: String,
}

impl TavilyIntegration {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            client: reqwest::Client::new(),
        }
    }

    async fn tavily_search(&self, query: &str) -> anyhow::Result<String> {
        tracing::info!("Searching Tavily for: {}", query);

        let body = serde_json::json!({
            "query": query,
            "max_results": 10,
            "search_depth": "basic",
        });

        let resp = self
            .client
            .post("https://api.tavily.com/search")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Ok(format!(
                "### Tavily Search Failed\nStatus {}: {}",
                status, err_body
            ));
        }

        let data: TavilyResponse = resp.json().await?;

        if data.results.is_empty() {
            return Ok("### No Results\nTavily returned no results for this query.".into());
        }

        let mut output = String::from("### Search Results\n\n");
        for (i, r) in data.results.iter().enumerate() {
            output.push_str(&format!(
                "{}. **{}**\n   {}\n   {}\n\n",
                i + 1,
                r.title,
                r.url,
                r.content
            ));
        }

        Ok(output)
    }
}

#[async_trait::async_trait]
impl Integration for TavilyIntegration {
    fn name(&self) -> &str {
        "tavily"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "tavily_search".to_string(),
            description: "Searches the web using Tavily, returning structured results with titles, URLs, and content snippets.".to_string(),
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
        }]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        let args: serde_json::Value = serde_json::from_str(arguments)?;

        match tool_name {
            "tavily_search" => {
                let query = args["query"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing query"))?;
                self.tavily_search(query).await
            }
            _ => anyhow::bail!("Unknown tool: {}", tool_name),
        }
    }
}
