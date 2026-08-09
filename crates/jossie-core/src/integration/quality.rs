#[derive(Debug, Clone, PartialEq)]
pub enum ResultQuality {
    Good,
    Empty,
    Partial,
    PossibleError,
}

pub fn validate_tool_result(tool_name: &str, content: &str) -> (ResultQuality, Option<String>) {
    let trimmed = content.trim();

    // Check for empty results
    if trimmed.is_empty()
        || trimmed == "[]"
        || trimmed == "{}"
        || trimmed == "null"
        || trimmed == "\"\""
    {
        return (
            ResultQuality::Empty,
            Some(format!(
                "[HINT: {tool_name} returned empty results. Consider trying different search terms or parameters.]"
            )),
        );
    }

    // Check for HTTP error patterns
    if trimmed.contains("403 Forbidden")
        || trimmed.contains("401 Unauthorized")
        || trimmed.contains("404 Not Found")
    {
        return (
            ResultQuality::PossibleError,
            Some(format!(
                "[HINT: {tool_name} returned an HTTP error. The resource may be inaccessible or the URL may be wrong.]"
            )),
        );
    }
    if trimmed.contains("500 Internal Server Error") || trimmed.contains("503 Service Unavailable")
    {
        return (
            ResultQuality::PossibleError,
            Some(format!(
                "[HINT: {tool_name} hit a server error. This may be transient - consider retrying later.]"
            )),
        );
    }

    // Check for truncation markers
    if trimmed.contains("[NOTICE: Output truncated") {
        return (
            ResultQuality::Partial,
            Some(format!(
                "[HINT: {tool_name} output was truncated. If the information you need is missing, consider a more narrow query.]"
            )),
        );
    }

    // Check for common error prefixes
    if trimmed.starts_with("Error:") || trimmed.starts_with("error:") {
        return (
            ResultQuality::PossibleError,
            Some(format!(
                "[HINT: {tool_name} returned an error. Review the error message and adjust your approach.]"
            )),
        );
    }

    (ResultQuality::Good, None)
}

// --- Error Recovery (#3) ---

#[derive(Debug, Clone, PartialEq)]
pub enum ToolErrorKind {
    Transient,
    BadInput,
    NotFound,
    AuthFailure,
    Unknown,
}

pub fn classify_error(error_msg: &str) -> ToolErrorKind {
    let lower = error_msg.to_lowercase();

    // Transient errors - safe to retry
    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("rate limit")
        || lower.contains("429")
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("temporarily unavailable")
    {
        return ToolErrorKind::Transient;
    }

    // Auth failures
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("authentication")
        || lower.contains("token expired")
    {
        return ToolErrorKind::AuthFailure;
    }

    // Not found
    if lower.contains("404") || lower.contains("not found") || lower.contains("no such") {
        return ToolErrorKind::NotFound;
    }

    // Bad input
    if lower.contains("invalid")
        || lower.contains("bad request")
        || lower.contains("400")
        || lower.contains("missing required")
        || lower.contains("malformed")
        || lower.contains("parse error")
    {
        return ToolErrorKind::BadInput;
    }

    ToolErrorKind::Unknown
}
