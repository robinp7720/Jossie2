use headless_chrome::{Browser, LaunchOptions, Tab};
use jossie_core::integration::{Integration, ToolDefinition};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OnceCell, RwLock};
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

fn is_bot_blocked(content: &str) -> bool {
    let lower = content.to_lowercase();
    BOT_BLOCK_PATTERNS.iter().any(|p| lower.contains(p))
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

    if let Ok(url) = Url::parse(&candidate) {
        if url.domain() == Some("duckduckgo.com") && url.path() == "/l/" {
            if let Some(target) = url
                .query_pairs()
                .find_map(|(key, value)| (key == "uddg").then_some(value.into_owned()))
            {
                return target;
            }
        }
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

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BrowserOpenSessionArgs {
    url: String,
    #[serde(default)]
    wait_for: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BrowserSessionSnapshotArgs {
    session_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BrowserNavigateArgs {
    session_id: String,
    url: String,
    #[serde(default)]
    wait_for: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BrowserCloseSessionArgs {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct BrowserMutationResult {
    ok: bool,
    #[serde(default)]
    message: Option<String>,
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
    browser: Arc<OnceCell<Browser>>,
    sessions: Arc<RwLock<HashMap<String, Arc<Tab>>>>,
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
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn shared_browser(&self) -> anyhow::Result<&Browser> {
        self.browser
            .get_or_try_init(|| async {
                tracing::info!("Launching shared headless browser instance");
                let options = LaunchOptions::default_builder()
                    .headless(true)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build launch options: {}", e))?;

                let browser = tokio::task::spawn_blocking(move || Browser::new(options))
                    .await
                    .map_err(|e| anyhow::anyhow!("Join error launching browser: {}", e))?
                    .map_err(|e| anyhow::anyhow!("Failed to launch browser: {}", e))?;

                Ok::<Browser, anyhow::Error>(browser)
            })
            .await
    }

    fn snapshot_script() -> &'static str {
        r#"
(() => {
  const collapse = (value) => (value || '').replace(/\s+/g, ' ').trim();
  const visible = (el) => {
    if (!el || !(el instanceof Element)) return false;
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
    if (el.hasAttribute('hidden')) return false;
    return !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length || style.position === 'fixed');
  };
  const selectorFor = (el) => {
    if (!el) return null;
    if (el.id) return `#${CSS.escape(el.id)}`;
    const name = el.getAttribute('name');
    if (name) {
      const byName = `${el.tagName.toLowerCase()}[name="${CSS.escape(name)}"]`;
      if (document.querySelectorAll(byName).length === 1) return byName;
    }
    const parts = [];
    let current = el;
    while (current && current.nodeType === Node.ELEMENT_NODE && current !== document.body) {
      let part = current.tagName.toLowerCase();
      const parent = current.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter((node) => node.tagName === current.tagName);
        if (siblings.length > 1) {
          part += `:nth-of-type(${siblings.indexOf(current) + 1})`;
        }
      }
      parts.unshift(part);
      current = current.parentElement;
    }
    return parts.length ? `body > ${parts.join(' > ')}` : 'body';
  };
  const labelFor = (el) => collapse(
    (el.labels && el.labels.length
      ? Array.from(el.labels).map((label) => label.innerText).join(' ')
      : '') || el.getAttribute('aria-label') || ''
  );
  const actionText = (el) => collapse(el.innerText || el.textContent || el.value || el.getAttribute('aria-label') || '');
  const summarizeField = (el) => ({
    selector: selectorFor(el),
    tag: el.tagName.toLowerCase(),
    input_type: el.getAttribute('type'),
    id: el.id || null,
    name: el.getAttribute('name'),
    label: labelFor(el) || null,
    placeholder: el.getAttribute('placeholder'),
    text: null,
    href: null,
    value_state: el.value ? 'set' : 'empty',
    selected_value: null,
    options: null,
  });
  const summarizeSelect = (el) => ({
    selector: selectorFor(el),
    tag: el.tagName.toLowerCase(),
    input_type: null,
    id: el.id || null,
    name: el.getAttribute('name'),
    label: labelFor(el) || null,
    placeholder: null,
    text: null,
    href: null,
    value_state: null,
    selected_value: el.value || null,
    options: Array.from(el.options || []).slice(0, 12).map((option) => ({
      text: collapse(option.textContent || ''),
      value: option.value || '',
      selected: !!option.selected,
    })),
  });
  const summarizeAction = (el) => ({
    selector: selectorFor(el),
    tag: el.tagName.toLowerCase(),
    input_type: el.getAttribute('type'),
    id: el.id || null,
    name: el.getAttribute('name'),
    label: labelFor(el) || null,
    placeholder: null,
    text: actionText(el) || null,
    href: el.getAttribute('href'),
    value_state: null,
    selected_value: null,
    options: null,
  });
  const inputs = Array.from(document.querySelectorAll('input, textarea'))
    .filter((el) => visible(el) && (el.tagName.toLowerCase() !== 'input' || (el.getAttribute('type') || 'text').toLowerCase() !== 'hidden'))
    .slice(0, 20)
    .map(summarizeField);
  const selects = Array.from(document.querySelectorAll('select'))
    .filter(visible)
    .slice(0, 20)
    .map(summarizeSelect);
  const actions = Array.from(document.querySelectorAll('a[href], button, input[type="submit"], input[type="button"], [role="button"]'))
    .filter(visible)
    .slice(0, 30)
    .map(summarizeAction);

  return {
    url: window.location.href,
    title: document.title || '',
    text_preview: collapse(document.body ? document.body.innerText : '').slice(0, 4000),
    inputs,
    selects,
    actions,
  };
})()
"#
    }

    fn eval_json_sync<T>(tab: &Tab, script: &str, await_promise: bool) -> anyhow::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let value = tab
            .evaluate(script, await_promise)?
            .value
            .ok_or_else(|| anyhow::anyhow!("Browser script did not return a JSON value"))?;
        Ok(serde_json::from_value(value)?)
    }

    fn capture_snapshot_sync(tab: &Tab) -> anyhow::Result<BrowserPageSnapshot> {
        Self::eval_json_sync(tab, Self::snapshot_script(), false)
    }

    async fn session_tab(&self, session_id: &str) -> anyhow::Result<Arc<Tab>> {
        self.sessions
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Unknown browser session '{}'", session_id))
    }

    fn format_session_snapshot(
        session_id: &str,
        snapshot: BrowserPageSnapshot,
    ) -> anyhow::Result<String> {
        let mut value = serde_json::to_value(snapshot)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Browser snapshot was not an object"))?;
        object.insert(
            "session_id".to_string(),
            serde_json::Value::String(session_id.to_string()),
        );
        Ok(serde_json::to_string_pretty(&value)?)
    }

    fn navigate_tab_sync(
        tab: Arc<Tab>,
        url: String,
        wait_for: Option<String>,
    ) -> anyhow::Result<BrowserPageSnapshot> {
        tracing::info!("Navigating browser tab to {}", url);
        tab.navigate_to(&url)
            .map_err(|e| anyhow::anyhow!("Failed to navigate: {}", e))?;
        tab.wait_until_navigated()
            .map_err(|e| anyhow::anyhow!("Failed to wait for navigation: {}", e))?;

        if let Some(selector) = wait_for.as_deref() {
            tracing::info!("Waiting for selector in session: {}", selector);
            tab.wait_for_element(selector)
                .map_err(|e| anyhow::anyhow!("Failed waiting for selector '{}': {}", selector, e))?;
        }

        Self::capture_snapshot_sync(&tab)
    }

    fn run_fill_input_sync(
        tab: Arc<Tab>,
        args: &BrowserFillInputArgs,
    ) -> anyhow::Result<BrowserPageSnapshot> {
        let args_json = serde_json::to_string(args)?;
        let script = format!(
            r#"
(() => {{
  const args = {args_json};
  const normalize = (value) => (value || '').replace(/\s+/g, ' ').trim().toLowerCase();
  const visible = (el) => {{
    if (!el || !(el instanceof Element)) return false;
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
    if (el.hasAttribute('hidden')) return false;
    return !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length || style.position === 'fixed');
  }};
  const labelFor = (el) => ((el.labels && el.labels.length
      ? Array.from(el.labels).map((label) => label.innerText).join(' ')
      : '') || el.getAttribute('aria-label') || '');
  const candidates = Array.from(document.querySelectorAll('input, textarea'))
    .filter((el) => visible(el) && (el.tagName.toLowerCase() !== 'input' || (el.getAttribute('type') || 'text').toLowerCase() !== 'hidden'));
  const locate = () => {{
    if (args.selector) return document.querySelector(args.selector);
    const matchers = [
      ['id', (el) => el.id || ''],
      ['name', (el) => el.getAttribute('name') || ''],
      ['label', labelFor],
      ['placeholder', (el) => el.getAttribute('placeholder') || ''],
    ];
    for (const [key, getter] of matchers) {{
      if (!args[key]) continue;
      const expected = normalize(args[key]);
      const exact = candidates.find((el) => normalize(getter(el)) === expected);
      if (exact) return exact;
      const partial = candidates.find((el) => normalize(getter(el)).includes(expected));
      if (partial) return partial;
    }}
    return null;
  }};
  const target = locate();
  if (!target) {{
    return {{ ok: false, message: 'No matching input or textarea found' }};
  }}
  target.focus();
  target.value = args.value;
  target.dispatchEvent(new Event('input', {{ bubbles: true }}));
  target.dispatchEvent(new Event('change', {{ bubbles: true }}));
  return {{
    ok: true,
    message: 'Filled input',
  }};
}})()
"#
        );

        let result: BrowserMutationResult = Self::eval_json_sync(&tab, &script, false)?;
        if !result.ok {
            anyhow::bail!(
                "{}",
                result
                    .message
                    .unwrap_or_else(|| "Browser input fill failed".to_string())
            );
        }
        Self::capture_snapshot_sync(&tab)
    }

    fn run_click_sync(tab: Arc<Tab>, args: &BrowserClickArgs) -> anyhow::Result<BrowserPageSnapshot> {
        let args_json = serde_json::to_string(args)?;
        let script = format!(
            r#"
(() => {{
  const args = {args_json};
  const normalize = (value) => (value || '').replace(/\s+/g, ' ').trim().toLowerCase();
  const visible = (el) => {{
    if (!el || !(el instanceof Element)) return false;
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
    if (el.hasAttribute('hidden')) return false;
    return !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length || style.position === 'fixed');
  }};
  const actionText = (el) => (el.innerText || el.textContent || el.value || el.getAttribute('aria-label') || '');
  const candidates = Array.from(document.querySelectorAll('a[href], button, input[type="submit"], input[type="button"], [role="button"]'))
    .filter(visible)
    .filter((el) => !args.tag || el.tagName.toLowerCase() === args.tag.toLowerCase());
  const target = args.selector
    ? document.querySelector(args.selector)
    : candidates.find((el) => normalize(actionText(el)) === normalize(args.text || ''))
        || candidates.find((el) => normalize(actionText(el)).includes(normalize(args.text || '')));
  if (!target) {{
    return {{ ok: false, message: 'No matching clickable element found' }};
  }}
  target.click();
  return {{
    ok: true,
    message: 'Clicked element',
  }};
}})()
"#
        );

        let result: BrowserMutationResult = Self::eval_json_sync(&tab, &script, false)?;
        if !result.ok {
            anyhow::bail!(
                "{}",
                result
                    .message
                    .unwrap_or_else(|| "Browser click failed".to_string())
            );
        }
        std::thread::sleep(Duration::from_millis(args.wait_after_ms));
        Self::capture_snapshot_sync(&tab)
    }

    fn run_select_option_sync(
        tab: Arc<Tab>,
        args: &BrowserSelectOptionArgs,
    ) -> anyhow::Result<BrowserPageSnapshot> {
        let args_json = serde_json::to_string(args)?;
        let script = format!(
            r#"
(() => {{
  const args = {args_json};
  const normalize = (value) => (value || '').replace(/\s+/g, ' ').trim().toLowerCase();
  const visible = (el) => {{
    if (!el || !(el instanceof Element)) return false;
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
    if (el.hasAttribute('hidden')) return false;
    return !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length || style.position === 'fixed');
  }};
  const labelFor = (el) => ((el.labels && el.labels.length
      ? Array.from(el.labels).map((label) => label.innerText).join(' ')
      : '') || el.getAttribute('aria-label') || '');
  const candidates = Array.from(document.querySelectorAll('select')).filter(visible);
  const locate = () => {{
    if (args.selector) return document.querySelector(args.selector);
    const matchers = [
      ['id', (el) => el.id || ''],
      ['name', (el) => el.getAttribute('name') || ''],
      ['label', labelFor],
    ];
    for (const [key, getter] of matchers) {{
      if (!args[key]) continue;
      const expected = normalize(args[key]);
      const exact = candidates.find((el) => normalize(getter(el)) === expected);
      if (exact) return exact;
      const partial = candidates.find((el) => normalize(getter(el)).includes(expected));
      if (partial) return partial;
    }}
    return null;
  }};
  const select = locate();
  if (!select) {{
    return {{ ok: false, message: 'No matching select element found' }};
  }}
  let option = null;
  if (args.value) {{
    option = Array.from(select.options).find((candidate) => candidate.value === args.value);
  }}
  if (!option && args.text) {{
    option = Array.from(select.options).find((candidate) => normalize(candidate.textContent || '') === normalize(args.text))
      || Array.from(select.options).find((candidate) => normalize(candidate.textContent || '').includes(normalize(args.text)));
  }}
  if (!option) {{
    return {{ ok: false, message: 'No matching select option found' }};
  }}
  select.value = option.value;
  option.selected = true;
  select.dispatchEvent(new Event('input', {{ bubbles: true }}));
  select.dispatchEvent(new Event('change', {{ bubbles: true }}));
  return {{
    ok: true,
    message: 'Selected option',
  }};
}})()
"#
        );

        let result: BrowserMutationResult = Self::eval_json_sync(&tab, &script, false)?;
        if !result.ok {
            anyhow::bail!(
                "{}",
                result
                    .message
                    .unwrap_or_else(|| "Browser select failed".to_string())
            );
        }
        std::thread::sleep(Duration::from_millis(args.wait_after_ms));
        Self::capture_snapshot_sync(&tab)
    }

    async fn browser_open_session(
        &self,
        url: &str,
        wait_for: Option<&str>,
    ) -> anyhow::Result<String> {
        let browser = self.shared_browser().await?;
        let tab = browser
            .new_tab()
            .map_err(|e| anyhow::anyhow!("Failed to open browser tab: {}", e))?;
        let session_id = Uuid::new_v4().to_string();
        let url = url.to_string();
        let wait_for = wait_for.map(|value| value.to_string());
        let session_tab = tab.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            Self::navigate_tab_sync(session_tab, url, wait_for)
        })
            .await
            .map_err(|e| anyhow::anyhow!("Join error opening session: {}", e))??;

        self.sessions.write().await.insert(session_id.clone(), tab);
        Self::format_session_snapshot(&session_id, snapshot)
    }

    async fn browser_session_snapshot(&self, session_id: &str) -> anyhow::Result<String> {
        let tab = self.session_tab(session_id).await?;
        let snapshot = tokio::task::spawn_blocking(move || Self::capture_snapshot_sync(&tab))
            .await
            .map_err(|e| anyhow::anyhow!("Join error capturing snapshot: {}", e))??;
        Self::format_session_snapshot(session_id, snapshot)
    }

    async fn browser_navigate(
        &self,
        session_id: &str,
        url: &str,
        wait_for: Option<&str>,
    ) -> anyhow::Result<String> {
        let tab = self.session_tab(session_id).await?;
        let url = url.to_string();
        let wait_for = wait_for.map(|value| value.to_string());
        let snapshot = tokio::task::spawn_blocking(move || Self::navigate_tab_sync(tab, url, wait_for))
            .await
            .map_err(|e| anyhow::anyhow!("Join error navigating session: {}", e))??;
        Self::format_session_snapshot(session_id, snapshot)
    }

    async fn browser_fill_input(&self, args: &BrowserFillInputArgs) -> anyhow::Result<String> {
        let tab = self.session_tab(&args.session_id).await?;
        let session_id = args.session_id.clone();
        let action_args = args.clone();
        let snapshot =
            tokio::task::spawn_blocking(move || Self::run_fill_input_sync(tab, &action_args))
            .await
            .map_err(|e| anyhow::anyhow!("Join error filling browser input: {}", e))??;
        Self::format_session_snapshot(&session_id, snapshot)
    }

    async fn browser_click(&self, args: &BrowserClickArgs) -> anyhow::Result<String> {
        let tab = self.session_tab(&args.session_id).await?;
        let session_id = args.session_id.clone();
        let action_args = args.clone();
        let snapshot = tokio::task::spawn_blocking(move || Self::run_click_sync(tab, &action_args))
            .await
            .map_err(|e| anyhow::anyhow!("Join error clicking browser element: {}", e))??;
        Self::format_session_snapshot(&session_id, snapshot)
    }

    async fn browser_select_option(
        &self,
        args: &BrowserSelectOptionArgs,
    ) -> anyhow::Result<String> {
        let tab = self.session_tab(&args.session_id).await?;
        let session_id = args.session_id.clone();
        let action_args = args.clone();
        let snapshot =
            tokio::task::spawn_blocking(move || Self::run_select_option_sync(tab, &action_args))
                .await
                .map_err(|e| anyhow::anyhow!("Join error selecting browser option: {}", e))??;
        Self::format_session_snapshot(&session_id, snapshot)
    }

    async fn browser_close_session(&self, session_id: &str) -> anyhow::Result<String> {
        let tab = self
            .sessions
            .write()
            .await
            .remove(session_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown browser session '{}'", session_id))?;
        tokio::task::spawn_blocking(move || tab.close(true))
            .await
            .map_err(|e| anyhow::anyhow!("Join error closing browser session: {}", e))?
            .map_err(|e| anyhow::anyhow!("Failed to close browser session: {}", e))?;
        Ok(format!("Closed browser session '{}'", session_id))
    }

    /// Launch or reuse the shared browser, then open a new tab.
    async fn browser_render(
        &self,
        url_str: &str,
        selector: Option<&str>,
    ) -> anyhow::Result<String> {
        let browser = self.shared_browser().await?;

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
                    anyhow::bail!(
                        "browser_select_option requires selector, id, name, or label"
                    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_duckduckgo_challenge_pages() {
        let challenge = "Unfortunately, bots use DuckDuckGo too. Please confirm this search was made by a human.";
        assert!(is_bot_blocked(challenge));
    }

    #[test]
    fn parses_duckduckgo_lite_results_and_decodes_redirects() {
        let html = r#"
            <html><body>
              <table>
                <tr>
                  <td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&amp;rut=abc" class='result-link'>Rust Programming Language</a></td>
                </tr>
                <tr><td class='result-snippet'>Rust is blazingly fast and memory-efficient.</td></tr>
                <tr>
                  <td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F&amp;rut=def" class='result-link'>The Rust Programming Language</a></td>
                </tr>
                <tr><td class='result-snippet'>Read the book.</td></tr>
              </table>
            </body></html>
        "#;

        let results = parse_duckduckgo_lite_results(html);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://rust-lang.org/");
        assert_eq!(
            results[0].snippet,
            "Rust is blazingly fast and memory-efficient."
        );
    }

    #[test]
    fn parses_brave_results() {
        let html = r#"
            <html><body>
              <div class="snippet" data-type="web">
                <div class="result-content">
                  <a href="https://rust-lang.org/" target="_self">
                    <div class="title search-snippet-title" title="Rust Programming Language">Rust Programming Language</div>
                  </a>
                  <div class="generic-snippet">
                    <div class="content">Rust is blazingly fast and memory-efficient.</div>
                  </div>
                </div>
              </div>
            </body></html>
        "#;

        let results = parse_brave_results(html);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://rust-lang.org/");
        assert_eq!(
            results[0].snippet,
            "Rust is blazingly fast and memory-efficient."
        );
    }
}
