impl BrowserIntegration {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .expect("Failed to build reqwest client");

        Self {
            client,
            browser: Arc::new(RwLock::new(None)),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn launch_browser() -> anyhow::Result<Browser> {
        tracing::info!(
            "Launching shared headless browser instance with idle timeout of {:?}",
            BROWSER_IDLE_TIMEOUT
        );
        let options = LaunchOptions::default_builder()
            .headless(true)
            .idle_browser_timeout(BROWSER_IDLE_TIMEOUT)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build launch options: {}", e))?;

        tokio::task::spawn_blocking(move || Browser::new(options))
            .await
            .map_err(|e| anyhow::anyhow!("Join error launching browser: {}", e))?
            .map_err(|e| anyhow::anyhow!("Failed to launch browser: {}", e))
    }

    async fn shared_browser(&self) -> anyhow::Result<Browser> {
        if let Some(browser) = self.browser.read().await.as_ref().cloned() {
            return Ok(browser);
        }

        let browser = Self::launch_browser().await?;
        let mut slot = self.browser.write().await;
        if let Some(existing) = slot.as_ref().cloned() {
            return Ok(existing);
        }
        *slot = Some(browser.clone());
        Ok(browser)
    }

    async fn invalidate_browser_state(&self, reason: &str) {
        tracing::warn!("Resetting shared browser state: {}", reason);
        self.sessions.write().await.clear();
        self.browser.write().await.take();
    }

    async fn open_browser_tab(&self) -> anyhow::Result<Arc<Tab>> {
        for attempt in 0..=1 {
            let browser = self.shared_browser().await?;
            match browser.new_tab() {
                Ok(tab) => {
                    tab.set_default_timeout(TAB_DEFAULT_TIMEOUT);
                    return Ok(tab);
                }
                Err(err)
                    if attempt == 0 && is_browser_connection_closed_message(&err.to_string()) =>
                {
                    self.invalidate_browser_state(&format!(
                        "shared browser connection closed while opening a tab: {}",
                        err
                    ))
                    .await;
                }
                Err(err) => anyhow::bail!("Failed to open browser tab: {}", err),
            }
        }

        anyhow::bail!("Failed to recover shared browser after connection closure")
    }

    async fn run_session_operation<F>(
        &self,
        session_id: &str,
        action: &str,
        operation: F,
    ) -> anyhow::Result<BrowserPageSnapshot>
    where
        F: FnOnce(Arc<Tab>) -> anyhow::Result<BrowserPageSnapshot> + Send + 'static,
    {
        let tab = self.session_tab(session_id).await?;
        let result = tokio::task::spawn_blocking(move || operation(tab))
            .await
            .map_err(|e| anyhow::anyhow!("Join error while trying to {action}: {}", e))?;

        match result {
            Ok(snapshot) => Ok(snapshot),
            Err(err) if is_browser_connection_closed_message(&err.to_string()) => {
                self.invalidate_browser_state(&format!(
                    "browser session '{}' expired while trying to {}: {}",
                    session_id, action, err
                ))
                .await;
                anyhow::bail!(
                    "Browser session '{}' expired because the underlying browser connection closed while trying to {}. Open a new browser session and try again.",
                    session_id,
                    action
                );
            }
            Err(err) => Err(err),
        }
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
        let serialized_script = format!("JSON.stringify({script})");
        let value = tab
            .evaluate(&serialized_script, await_promise)?
            .value
            .ok_or_else(|| anyhow::anyhow!("Browser script did not return a JSON value"))?;
        let json = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Browser script did not return a JSON string"))?;
        Ok(serde_json::from_str(json)?)
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
            tab.wait_for_element(selector).map_err(|e| {
                anyhow::anyhow!("Failed waiting for selector '{}': {}", selector, e)
            })?;
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

    fn run_click_sync(
        tab: Arc<Tab>,
        args: &BrowserClickArgs,
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

}

impl Default for BrowserIntegration {
    fn default() -> Self {
        Self::new()
    }
}
