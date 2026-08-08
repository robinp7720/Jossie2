/// Strip HTML tags and collapse whitespace, producing approximate plain text.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag = String::new();
    let mut suppressed_element: Option<String> = None;
    let mut last_was_space = false;

    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' => {
                in_tag = false;
                let trimmed = tag.trim_start();
                let closing = trimmed.starts_with('/');
                let tag_name = trimmed
                    .trim_start_matches('/')
                    .split(|character: char| !character.is_ascii_alphanumeric())
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if suppressed_element.as_deref() == Some(tag_name.as_str()) && closing {
                    suppressed_element = None;
                } else if suppressed_element.is_none()
                    && !closing
                    && matches!(tag_name.as_str(), "style" | "script" | "head" | "template")
                {
                    suppressed_element = Some(tag_name);
                }
                if suppressed_element.is_none() && !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            _ if in_tag => tag.push(ch),
            _ if suppressed_element.is_some() => {}
            _ => {
                let normalized = if ch.is_whitespace() { ' ' } else { ch };
                if normalized == ' ' {
                    if !last_was_space {
                        out.push(' ');
                        last_was_space = true;
                    }
                } else {
                    out.push(normalized);
                    last_was_space = false;
                }
            }
        }
    }

    out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&euro;", "€")
        .replace("&#8364;", "€")
        .replace("&#x20ac;", "€")
        .replace("&#X20AC;", "€");

    out.trim().to_string()
}

/// Truncate text to `max_chars` and append a notice if truncated.
pub fn truncate_with_notice(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }

    let truncated: String = text.chars().take(max_chars).collect();
    format!(
        "{}\n\n[Message truncated to {} characters]",
        truncated, max_chars
    )
}

/// Count approximate visible (non-whitespace, non-tag) characters in HTML.
pub fn approx_visible_len(html: &str) -> usize {
    html_to_text(html)
        .chars()
        .filter(|character| !character.is_whitespace())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_strips_tags() {
        assert_eq!(html_to_text("<p>Hello</p>"), "Hello");
    }

    #[test]
    fn html_to_text_collapses_whitespace() {
        assert_eq!(html_to_text("hello   world"), "hello world");
    }

    #[test]
    fn html_to_text_decodes_entities() {
        assert_eq!(html_to_text("&amp; &lt; &gt;"), "& < >");
    }

    #[test]
    fn html_to_text_removes_non_visible_document_content() {
        let html = "<html><head><style>body { color: red; }</style><script>track()</script></head><body><p>Paid &euro;12.34</p><template>hidden</template></body></html>";
        assert_eq!(html_to_text(html), "Paid €12.34");
        assert_eq!(approx_visible_len(html), 10);
    }

    #[test]
    fn truncate_with_notice_short() {
        let s = "hello".to_string();
        assert_eq!(truncate_with_notice(s, 10), "hello");
    }

    #[test]
    fn truncate_with_notice_long() {
        let s = "hello world".to_string();
        let result = truncate_with_notice(s, 5);
        assert!(result.starts_with("hello"));
        assert!(result.contains("[Message truncated"));
    }

    #[test]
    fn approx_visible_len_basic() {
        assert_eq!(approx_visible_len("<b>hi</b>"), 2);
    }
}
