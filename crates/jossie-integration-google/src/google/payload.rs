fn summarize_structure(payload: &serde_json::Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let mime = payload
        .get("mimeType")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown");
    let has_data = payload.pointer("/body/data").is_some();
    let att_id = payload.pointer("/body/attachmentId").is_some();
    let size = payload
        .pointer("/body/size")
        .and_then(|s| s.as_u64())
        .unwrap_or(0);

    let mut out = format!(
        "{}Mime: {}, size: {}, has_data: {}, has_att_id: {}\n",
        indent, mime, size, has_data, att_id
    );

    if let Some(parts) = payload.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            out.push_str(&summarize_structure(part, depth + 1));
        }
    }
    out
}
fn decode_base64_url(data: &str) -> Option<String> {
    decode_base64_url_bytes(data).map(|decoded| String::from_utf8_lossy(&decoded).to_string())
}

fn decode_base64_url_bytes(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;

    // Gmail sometimes includes line breaks or padding; normalize before decode.
    let cleaned: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();

    let try_decode = |engine: base64::engine::general_purpose::GeneralPurpose| {
        engine.decode(cleaned.as_bytes()).ok()
    };

    try_decode(base64::engine::general_purpose::URL_SAFE_NO_PAD)
        .or_else(|| try_decode(base64::engine::general_purpose::URL_SAFE))
        .or_else(|| try_decode(base64::engine::general_purpose::STANDARD_NO_PAD))
        .or_else(|| try_decode(base64::engine::general_purpose::STANDARD))
}

async fn extract_body_from_payload(
    client: &reqwest::Client,
    token: &str,
    message_id: &str,
    payload: &serde_json::Value,
    debug: bool,
) -> String {
    let text = extract_content(client, token, message_id, payload, "text/plain", debug)
        .await
        .unwrap_or_default();
    let html = extract_content(client, token, message_id, payload, "text/html", debug)
        .await
        .unwrap_or_default();

    choose_preferred_body(text, html)
}

fn choose_preferred_body(text: String, html: String) -> String {
    let text_trimmed = text.trim();
    let html_trimmed = html.trim();
    if text_trimmed.is_empty() && !html_trimmed.is_empty() {
        return jossie_core::text::html_to_text(&html);
    }
    if html_trimmed.is_empty() {
        return text;
    }

    let text_len = text_trimmed.len();
    let html_visible_len = jossie_core::text::approx_visible_len(html_trimmed);

    // Prefer HTML if it contains substantially more visible content.
    if html_visible_len > text_len.saturating_mul(2)
        || (text_len < 200 && html_visible_len > text_len)
    {
        return jossie_core::text::html_to_text(&html);
    }

    text
}

fn collect_attachments(payload: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut stack = vec![payload];

    while let Some(part) = stack.pop() {
        if let Some(parts) = part.get("parts").and_then(|p| p.as_array()) {
            for child in parts.iter().rev() {
                stack.push(child);
            }
        }

        let filename = part.get("filename").and_then(|f| f.as_str()).unwrap_or("");
        let mime = part.get("mimeType").and_then(|m| m.as_str()).unwrap_or("");
        let attachment_id = part.pointer("/body/attachmentId").and_then(|a| a.as_str());
        let size = part
            .pointer("/body/size")
            .and_then(|s| s.as_u64())
            .unwrap_or(0);

        let is_non_text = !mime.to_lowercase().starts_with("text/");
        let has_payload = attachment_id.is_some() || size > 0;

        if (is_non_text && has_payload) || (!filename.is_empty() && has_payload) {
            out.push(serde_json::json!({
                "filename": if filename.is_empty() { None::<String> } else { Some(filename.to_string()) },
                "mimeType": if mime.is_empty() { None::<String> } else { Some(mime.to_string()) },
                "size": size,
                "attachmentId": attachment_id.map(|a| a.to_string()),
            }));
        }
    }

    out
}

async fn extract_content(
    client: &reqwest::Client,
    token: &str,
    message_id: &str,
    payload: &serde_json::Value,
    target_mime: &str,
    debug: bool,
) -> Option<String> {
    let mut stack = vec![payload];
    while let Some(part) = stack.pop() {
        if let Some(mime) = part.get("mimeType").and_then(|m| m.as_str())
            && mime.to_lowercase().starts_with(target_mime)
        {
            if let Some(data) = part.pointer("/body/data").and_then(|d| d.as_str()) {
                if let Some(decoded) = decode_base64_url(data) {
                    return Some(decoded);
                }
                log_decode_failure("body.data", message_id, mime, data, debug);
            }
            if let Some(att_id) = part.pointer("/body/attachmentId").and_then(|a| a.as_str())
                && let Some(decoded) =
                    fetch_attachment_text(client, token, message_id, att_id, mime, debug).await
            {
                return Some(decoded);
            }
        }
        if let Some(parts) = part.get("parts").and_then(|p| p.as_array()) {
            for child in parts.iter().rev() {
                stack.push(child);
            }
        }
    }
    None
}

async fn fetch_attachment_text(
    client: &reqwest::Client,
    token: &str,
    message_id: &str,
    attachment_id: &str,
    mime: &str,
    debug: bool,
) -> Option<String> {
    if attachment_id.trim().is_empty() {
        return None;
    }

    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}/attachments/{attachment_id}"
    );
    let resp = match client.get(&url).bearer_auth(token).send().await {
        Ok(resp) => resp,
        Err(err) => {
            tracing::warn!("Gmail attachment fetch failed for {}: {}", message_id, err);
            return None;
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            "Gmail attachment fetch failed for {} ({}): {}",
            message_id,
            status,
            body
        );
        return None;
    }

    let data: serde_json::Value = match resp.json().await {
        Ok(json) => json,
        Err(err) => {
            tracing::warn!(
                "Gmail attachment JSON parse failed for {}: {}",
                message_id,
                err
            );
            return None;
        }
    };

    let raw = data.get("data").and_then(|d| d.as_str())?;
    let decoded = decode_base64_url(raw);
    if decoded.is_none() {
        log_decode_failure("attachment.data", message_id, mime, raw, debug);
    }
    decoded
}

fn log_decode_failure(source: &str, message_id: &str, mime: &str, raw: &str, debug: bool) {
    if !debug {
        return;
    }

    let mut whitespace = 0usize;
    let mut invalid = 0usize;
    let mut url_safe = 0usize;
    let mut cleaned_len = 0usize;
    let mut prefix = String::new();

    for ch in raw.chars() {
        if ch.is_ascii_whitespace() {
            whitespace += 1;
            continue;
        }
        cleaned_len += 1;
        if prefix.len() < 12 {
            prefix.push(ch);
        }
        let valid = ch.is_ascii_alphanumeric()
            || ch == '+'
            || ch == '/'
            || ch == '='
            || ch == '-'
            || ch == '_';
        if !valid {
            invalid += 1;
        }
        if ch == '-' || ch == '_' {
            url_safe += 1;
        }
    }

    let has_padding = raw.contains('=');
    let mime = if mime.is_empty() { "unknown" } else { mime };

    tracing::warn!(
        "Gmail base64 decode failed ({source}) msg={} mime={} len={} whitespace={} invalid={} urlsafe={} padding={} prefix={}",
        message_id,
        mime,
        cleaned_len,
        whitespace,
        invalid,
        url_safe,
        has_padding,
        prefix
    );
}

fn extract_event_time(value: Option<&serde_json::Value>) -> Option<String> {
    let v = value?;
    if let Some(dt) = v.get("dateTime").and_then(|x| x.as_str()) {
        return Some(dt.to_string());
    }
    if let Some(date) = v.get("date").and_then(|x| x.as_str()) {
        return Some(date.to_string());
    }
    None
}

fn build_calendar_update_body(update: CalendarEventUpdate) -> anyhow::Result<serde_json::Value> {
    let mut body = serde_json::Map::new();

    if let Some(summary) = non_empty_optional(update.summary.as_deref()) {
        body.insert(
            "summary".to_string(),
            serde_json::Value::String(summary.to_string()),
        );
    }

    if let Some(description) = update.description {
        body.insert(
            "description".to_string(),
            serde_json::Value::String(description),
        );
    }

    if let Some(location) = update.location {
        body.insert("location".to_string(), serde_json::Value::String(location));
    }

    let has_time = update
        .start_time
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
        || update
            .end_time
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
    let has_date = update
        .start_date
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
        || update
            .end_date
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());

    if has_time && has_date {
        anyhow::bail!("Use either start_time/end_time or start_date/end_date, not both");
    }

    if has_time {
        let start = non_empty_optional(update.start_time.as_deref())
            .ok_or_else(|| anyhow::anyhow!("start_time is required when end_time is set"))?;
        let end = non_empty_optional(update.end_time.as_deref())
            .ok_or_else(|| anyhow::anyhow!("end_time is required when start_time is set"))?;
        body.insert(
            "start".to_string(),
            serde_json::json!({ "dateTime": start }),
        );
        body.insert("end".to_string(), serde_json::json!({ "dateTime": end }));
    }

    if has_date {
        let start = non_empty_optional(update.start_date.as_deref())
            .ok_or_else(|| anyhow::anyhow!("start_date is required when end_date is set"))?;
        let end = non_empty_optional(update.end_date.as_deref())
            .ok_or_else(|| anyhow::anyhow!("end_date is required when start_date is set"))?;
        body.insert("start".to_string(), serde_json::json!({ "date": start }));
        body.insert("end".to_string(), serde_json::json!({ "date": end }));
    }

    if body.is_empty() {
        anyhow::bail!("At least one calendar event field must be provided");
    }

    Ok(serde_json::Value::Object(body))
}

fn non_empty_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn normalize_calendar_send_updates(value: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(value) = non_empty_optional(value) else {
        return Ok(Some("none".to_string()));
    };

    match value {
        "all" | "externalOnly" | "none" => Ok(Some(value.to_string())),
        other => anyhow::bail!(
            "send_updates must be one of 'all', 'externalOnly', or 'none', got '{}'",
            other
        ),
    }
}
