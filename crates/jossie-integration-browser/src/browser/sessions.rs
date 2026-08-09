impl BrowserIntegration {
    async fn browser_open_session(
        &self,
        url: &str,
        wait_for: Option<&str>,
    ) -> anyhow::Result<String> {
        let session_id = Uuid::new_v4().to_string();
        for attempt in 0..=1 {
            let tab = self.open_browser_tab().await?;
            let url = url.to_string();
            let wait_for = wait_for.map(|value| value.to_string());
            let session_tab = tab.clone();
            let result = tokio::task::spawn_blocking(move || {
                Self::navigate_tab_sync(session_tab, url, wait_for)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Join error opening session: {}", e))?;

            match result {
                Ok(snapshot) => {
                    self.sessions.write().await.insert(session_id.clone(), tab);
                    return Self::format_session_snapshot(&session_id, snapshot);
                }
                Err(err)
                    if attempt == 0 && is_browser_connection_closed_message(&err.to_string()) =>
                {
                    self.invalidate_browser_state(&format!(
                        "shared browser connection closed while opening session '{}': {}",
                        session_id, err
                    ))
                    .await;
                }
                Err(err) => return Err(err),
            }
        }

        anyhow::bail!("Failed to recover browser session startup after connection closure")
    }

    async fn browser_session_snapshot(&self, session_id: &str) -> anyhow::Result<String> {
        let snapshot = self
            .run_session_operation(session_id, "capture the current page snapshot", |tab| {
                Self::capture_snapshot_sync(&tab)
            })
            .await?;
        Self::format_session_snapshot(session_id, snapshot)
    }

    async fn browser_navigate(
        &self,
        session_id: &str,
        url: &str,
        wait_for: Option<&str>,
    ) -> anyhow::Result<String> {
        let url = url.to_string();
        let wait_for = wait_for.map(|value| value.to_string());
        let snapshot = self
            .run_session_operation(session_id, "navigate the page", move |tab| {
                Self::navigate_tab_sync(tab, url, wait_for)
            })
            .await?;
        Self::format_session_snapshot(session_id, snapshot)
    }

    async fn browser_fill_input(&self, args: &BrowserFillInputArgs) -> anyhow::Result<String> {
        let session_id = args.session_id.clone();
        let action_args = args.clone();
        let snapshot = self
            .run_session_operation(&session_id, "fill an input", move |tab| {
                Self::run_fill_input_sync(tab, &action_args)
            })
            .await?;
        Self::format_session_snapshot(&session_id, snapshot)
    }

    async fn browser_click(&self, args: &BrowserClickArgs) -> anyhow::Result<String> {
        let session_id = args.session_id.clone();
        let action_args = args.clone();
        let snapshot = self
            .run_session_operation(&session_id, "click an element", move |tab| {
                Self::run_click_sync(tab, &action_args)
            })
            .await?;
        Self::format_session_snapshot(&session_id, snapshot)
    }

    async fn browser_select_option(
        &self,
        args: &BrowserSelectOptionArgs,
    ) -> anyhow::Result<String> {
        let session_id = args.session_id.clone();
        let action_args = args.clone();
        let snapshot = self
            .run_session_operation(&session_id, "select an option", move |tab| {
                Self::run_select_option_sync(tab, &action_args)
            })
            .await?;
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

}
