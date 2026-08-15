use headless_chrome::{Browser, LaunchOptions, Tab};
use jossie_core::integration::{Integration, ToolDefinition};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use url::Url;
use uuid::Uuid;

/// Patterns that indicate a page blocked us (bot detection, CAPTCHA, etc.)
const BOT_BLOCK_PATTERNS: &[&str] = &[
    "unusual traffic",
    "please enable javascript",
    "captcha",
    "are not a robot",
    "blocked your ip",
    "access denied",
    "bots use duckduckgo too",
    "confirm this search was made by a human",
    "select all squares containing",
    "anomaly-modal",
    "challenge-form",
];

const SEARCH_RESULT_LIMIT: usize = 5;
const BROWSER_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const TAB_DEFAULT_TIMEOUT: Duration = Duration::from_secs(45);

fn is_bot_blocked(content: &str) -> bool {
    let lower = content.to_lowercase();
    BOT_BLOCK_PATTERNS.iter().any(|p| lower.contains(p))
}

fn is_browser_connection_closed_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("underlying connection is closed")
        || lower.contains("connection closed")
        || lower.contains("unable to make method calls because underlying connection is closed")
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("valid CSS selector")
}

fn extract_element_text(element: ElementRef<'_>) -> String {
    collapse_whitespace(&element.text().collect::<Vec<_>>().join(" "))
}

fn decode_duckduckgo_redirect(raw_href: &str) -> String {
    let candidate = if raw_href.starts_with("//") {
        format!("https:{raw_href}")
    } else {
        raw_href.to_string()
    };

    if let Ok(url) = Url::parse(&candidate)
        && url.domain() == Some("duckduckgo.com")
        && url.path() == "/l/"
        && let Some(target) = url
            .query_pairs()
            .find_map(|(key, value)| (key == "uddg").then_some(value.into_owned()))
    {
        return target;
    }

    candidate
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BrowserOptionSummary {
    text: String,
    value: String,
    selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BrowserElementSummary {
    selector: String,
    tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<BrowserOptionSummary>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BrowserPageSnapshot {
    url: String,
    title: String,
    text_preview: String,
    inputs: Vec<BrowserElementSummary>,
    selects: Vec<BrowserElementSummary>,
    actions: Vec<BrowserElementSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserReadPageArgs {
    /// The URL to visit.
    url: String,
    /// Optional CSS selector to focus on. If omitted, captures the whole body.
    #[schemars(required)]
    selector: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserOpenSessionArgs {
    url: String,
    #[serde(default)]
    wait_for: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserSessionSnapshotArgs {
    session_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserNavigateArgs {
    session_id: String,
    url: String,
    #[serde(default)]
    wait_for: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserFillInputArgs {
    session_id: String,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    placeholder: Option<String>,
    value: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserClickArgs {
    session_id: String,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default = "default_interaction_wait_ms")]
    wait_after_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserSelectOptionArgs {
    session_id: String,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default = "default_interaction_wait_ms")]
    wait_after_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserCloseSessionArgs {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct BrowserMutationResult {
    ok: bool,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserSearchArgs {
    /// The search query.
    query: String,
}

fn default_interaction_wait_ms() -> u64 {
    1200
}

impl SearchResult {
    fn new(title: String, url: String, snippet: String) -> Option<Self> {
        let title = collapse_whitespace(&title);
        let url = collapse_whitespace(&url);
        let snippet = collapse_whitespace(&snippet);

        if title.is_empty() || url.is_empty() {
            return None;
        }

        Some(Self {
            title,
            url,
            snippet,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum SearchProvider {
    DuckDuckGoLite,
    BraveHtml,
    DuckDuckGoInstantAnswer,
}

impl SearchProvider {
    fn label(self) -> &'static str {
        match self {
            Self::DuckDuckGoLite => "DuckDuckGo Lite",
            Self::BraveHtml => "Brave Search",
            Self::DuckDuckGoInstantAnswer => "DuckDuckGo Instant Answer",
        }
    }
}

fn parse_duckduckgo_lite_results(html: &str) -> Vec<SearchResult> {
    let document = Html::parse_document(html);
    let link_selector = selector("a.result-link");
    let snippet_selector = selector("td.result-snippet");
    let snippets = document
        .select(&snippet_selector)
        .map(extract_element_text)
        .collect::<Vec<_>>();

    document
        .select(&link_selector)
        .enumerate()
        .filter_map(|(index, link)| {
            let title = extract_element_text(link);
            let url = link.value().attr("href").map(decode_duckduckgo_redirect)?;
            let snippet = snippets.get(index).cloned().unwrap_or_default();
            SearchResult::new(title, url, snippet)
        })
        .collect()
}

fn parse_brave_results(html: &str) -> Vec<SearchResult> {
    let document = Html::parse_document(html);
    let snippet_selector = selector("div.snippet[data-type='web']");
    let link_selector = selector("a[href]");
    let title_selector = selector("div.title, a.title");
    let body_selector = selector("div.generic-snippet div.content, div.description");

    document
        .select(&snippet_selector)
        .filter_map(|snippet| {
            let link = snippet.select(&link_selector).next()?;
            let url = link.value().attr("href")?.to_string();
            let title = snippet
                .select(&title_selector)
                .next()
                .map(extract_element_text)
                .unwrap_or_default();
            let summary = snippet
                .select(&body_selector)
                .next()
                .map(extract_element_text)
                .unwrap_or_default();

            SearchResult::new(title, url, summary)
        })
        .collect()
}

fn format_search_results(
    query: &str,
    provider: SearchProvider,
    results: &[SearchResult],
) -> String {
    let mut out = vec![
        "### Search Results".to_string(),
        format!("Provider: {}", provider.label()),
        format!("Query: {query}"),
        String::new(),
    ];

    for (index, result) in results.iter().take(SEARCH_RESULT_LIMIT).enumerate() {
        out.push(format!("{}. {}", index + 1, result.title));
        out.push(format!("URL: {}", result.url));
        if !result.snippet.is_empty() {
            out.push(format!("Snippet: {}", result.snippet));
        }
        out.push(String::new());
    }

    out.join("\n").trim().to_string()
}

fn format_search_failures(query: &str, failures: &[String]) -> String {
    let mut out = vec![
        "### Search Failed".to_string(),
        format!("Query: {query}"),
        "Search providers did not return usable results.".to_string(),
        String::new(),
    ];

    if !failures.is_empty() {
        out.push("Attempts:".to_string());
        for failure in failures {
            out.push(format!("- {failure}"));
        }
    }

    out.join("\n")
}

fn collect_instant_answer_topics(items: &[serde_json::Value], out: &mut Vec<SearchResult>) {
    for item in items {
        if let Some(topics) = item.get("Topics").and_then(|topics| topics.as_array()) {
            collect_instant_answer_topics(topics, out);
            continue;
        }

        let Some(text) = item.get("Text").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(url) = item.get("FirstURL").and_then(|value| value.as_str()) else {
            continue;
        };

        let (title, snippet) = match text.split_once(" - ") {
            Some((title, snippet)) => (title.to_string(), snippet.to_string()),
            None => (text.to_string(), String::new()),
        };

        if let Some(result) = SearchResult::new(title, url.to_string(), snippet) {
            out.push(result);
        }
    }
}

pub struct BrowserIntegration {
    client: reqwest::Client,
    browser: Arc<RwLock<Option<Browser>>>,
    sessions: Arc<RwLock<HashMap<String, Arc<Tab>>>>,
}

include!("browser/lifecycle.rs");
include!("browser/sessions.rs");
include!("browser/pages.rs");
include!("browser/search.rs");
include!("browser/integration.rs");
include!("browser/tests.rs");
