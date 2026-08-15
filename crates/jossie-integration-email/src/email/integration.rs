#[async_trait::async_trait]
impl Integration for EmailIntegration {
    fn name(&self) -> &str {
        "email"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    async fn execute(&self, tool_name: &str, _arguments: &str) -> anyhow::Result<String> {
        anyhow::bail!("Unknown email tool: {tool_name}")
    }

    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        // If default config exists, we are good.
        if self.default_config.is_some() {
            return Ok(OnboardingStatus::Configured);
        }
        // If DB has accounts, we are good.
        if let Some(db) = &self.db {
            let accounts = db.list_integration_accounts("email").await?;
            if !accounts.is_empty() {
                return Ok(OnboardingStatus::Configured);
            }
        }

        // Otherwise, need setup
        Ok(OnboardingStatus::RequiresAction {
            fields: vec![
                OnboardingField {
                    name: "note".to_string(),
                    label: "Setup Email".to_string(),
                    input_type: "info".to_string(),
                    value: None,
                    description: Some("No email accounts configured. Add one via the settings API (implementation pending) or config.toml.".to_string()),
                }
            ]
        })
    }

    async fn poll(&self) -> anyhow::Result<()> {
        let Some(db) = &self.db else {
            return Ok(());
        };

        for account in self.list_poll_accounts().await? {
            if let Err(error) = self.poll_account(db, &account).await {
                tracing::warn!("IMAP poll failed for account {}: {}", account.id, error);
            }
        }

        Ok(())
    }
}
fn build_imap_search_query(
    legacy_query: Option<&str>,
    terms: &[String],
    match_mode: &str,
    from: Option<&str>,
    subject: Option<&str>,
    after: Option<&str>,
    before: Option<&str>,
) -> anyhow::Result<String> {
    let mut criteria = Vec::new();
    if let Some(after) = after.filter(|value| !value.trim().is_empty()) {
        let date = chrono::NaiveDate::parse_from_str(after, "%Y-%m-%d")?;
        criteria.push(format!("SINCE {}", date.format("%d-%b-%Y")));
    }
    if let Some(before) = before.filter(|value| !value.trim().is_empty()) {
        let date = chrono::NaiveDate::parse_from_str(before, "%Y-%m-%d")?;
        criteria.push(format!("BEFORE {}", date.format("%d-%b-%Y")));
    }
    if let Some(from) = from.filter(|value| !value.trim().is_empty()) {
        criteria.push(format!("FROM \"{}\"", escape_imap_query_value(from.trim())));
    }
    if let Some(subject) = subject.filter(|value| !value.trim().is_empty()) {
        criteria.push(format!(
            "SUBJECT \"{}\"",
            escape_imap_query_value(subject.trim())
        ));
    }

    let mut text_terms = terms
        .iter()
        .map(|term| term.trim())
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(query) = legacy_query.filter(|value| !value.trim().is_empty()) {
        text_terms.push(query.trim().to_string());
    }
    let mut term_criteria = text_terms
        .iter()
        .map(|term| format!("TEXT \"{}\"", escape_imap_query_value(term)))
        .collect::<Vec<_>>();
    if match_mode == "any" && term_criteria.len() > 1 {
        let mut combined = term_criteria.pop().unwrap_or_default();
        while let Some(item) = term_criteria.pop() {
            combined = format!("OR {item} {combined}");
        }
        criteria.push(combined);
    } else {
        criteria.extend(term_criteria);
    }

    if criteria.is_empty() {
        Ok("ALL".to_string())
    } else {
        Ok(criteria.join(" "))
    }
}

fn escape_imap_query_value(value: &str) -> String {
    value.replace('\\', r"\\").replace('"', r#"\""#)
}

fn extract_header_fields(header_bytes: &[u8]) -> (String, String, String) {
    match mailparse::parse_headers(header_bytes) {
        Ok((headers, _)) => (
            headers.get_first_value("From").unwrap_or_default(),
            headers.get_first_value("Subject").unwrap_or_default(),
            headers.get_first_value("Date").unwrap_or_default(),
        ),
        Err(_) => (String::new(), String::new(), String::new()),
    }
}

struct HeaderSummary {
    message_id: Option<String>,
    from: String,
    to: Vec<String>,
    subject: String,
    date: String,
}

fn parse_header_summary(header_bytes: &[u8]) -> HeaderSummary {
    match mailparse::parse_headers(header_bytes) {
        Ok((headers, _)) => HeaderSummary {
            message_id: headers.get_first_value("Message-ID"),
            from: headers.get_first_value("From").unwrap_or_default(),
            to: parse_recipient_list(&headers.get_first_value("To").unwrap_or_default()),
            subject: headers.get_first_value("Subject").unwrap_or_default(),
            date: headers.get_first_value("Date").unwrap_or_default(),
        },
        Err(_) => HeaderSummary {
            message_id: None,
            from: String::new(),
            to: Vec::new(),
            subject: String::new(),
            date: String::new(),
        },
    }
}

fn parse_recipient_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect()
}

fn truncate_with_notice(text: String, max_chars: usize) -> String {
    jossie_core::text::truncate_with_notice(text, max_chars)
}

fn text_fallback_preview(raw_message: &[u8]) -> String {
    let preview_len = raw_message.len().min(32_000);
    let preview = String::from_utf8_lossy(&raw_message[..preview_len]);
    // html_to_text also collapses whitespace, so it works on plain text too
    jossie_core::text::html_to_text(&preview)
}

fn extract_message_body(parsed: &ParsedMail<'_>) -> String {
    let mut text_parts = Vec::new();
    let mut html_parts = Vec::new();
    collect_message_parts(parsed, &mut text_parts, &mut html_parts);

    if !text_parts.is_empty() {
        return text_parts.join("\n\n").trim().to_string();
    }

    if !html_parts.is_empty() {
        return html_parts.join("\n\n").trim().to_string();
    }

    parsed.get_body().unwrap_or_default().trim().to_string()
}

fn collect_message_attachments(
    part: &ParsedMail<'_>,
    part_id: &str,
    attachments: &mut Vec<EmailAttachment>,
) {
    let disposition = part.get_content_disposition();
    let filename = disposition
        .params
        .get("filename")
        .or_else(|| part.ctype.params.get("name"))
        .cloned()
        .unwrap_or_default();
    let is_attachment = disposition.disposition == DispositionType::Attachment
        || (!filename.is_empty() && part.subparts.is_empty());

    if is_attachment {
        let size = part.get_body_raw().map(|body| body.len()).unwrap_or_default();
        attachments.push(EmailAttachment {
            part_id: part_id.to_string(),
            filename,
            mime_type: part.ctype.mimetype.to_ascii_lowercase(),
            size,
        });
        return;
    }

    for (index, child) in part.subparts.iter().enumerate() {
        let child_id = if part_id.is_empty() {
            (index + 1).to_string()
        } else {
            format!("{part_id}.{}", index + 1)
        };
        collect_message_attachments(child, &child_id, attachments);
    }
}

fn find_message_attachment<'a>(part: &'a ParsedMail<'a>, part_id: &str) -> Option<&'a ParsedMail<'a>> {
    let mut current = part;
    if part_id.is_empty() {
        return Some(current);
    }
    for segment in part_id.split('.') {
        let index = segment.parse::<usize>().ok()?.checked_sub(1)?;
        current = current.subparts.get(index)?;
    }
    Some(current)
}

fn collect_message_parts(
    part: &ParsedMail<'_>,
    text_parts: &mut Vec<String>,
    html_parts: &mut Vec<String>,
) {
    if part.get_content_disposition().disposition == DispositionType::Attachment {
        return;
    }

    if part.subparts.is_empty() {
        let mime = part.ctype.mimetype.to_ascii_lowercase();
        if mime == "text/plain" {
            if let Ok(body) = part.get_body() {
                let body = body.trim();
                if !body.is_empty() {
                    text_parts.push(body.to_string());
                }
            }
        } else if mime == "text/html" {
            if let Ok(body) = part.get_body() {
                let body = html_to_text(&body);
                if !body.is_empty() {
                    html_parts.push(body);
                }
            }
        }
        return;
    }

    for child in &part.subparts {
        collect_message_parts(child, text_parts, html_parts);
    }
}

fn html_to_text(html: &str) -> String {
    jossie_core::text::html_to_text(html)
}
