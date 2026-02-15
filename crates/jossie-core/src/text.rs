/// Strip HTML tags and collapse whitespace, producing approximate plain text.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut last_was_space = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            _ if in_tag => {}
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
        .replace("&#39;", "'");

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
    let mut in_tag = false;
    let mut count = 0usize;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {
                if !in_tag && !ch.is_whitespace() {
                    count += 1;
                }
            }
        }
    }
    count
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
