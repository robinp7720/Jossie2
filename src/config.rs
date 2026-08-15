use anyhow::Context;
use jossie_core::config::AppConfig;
use std::path::Path;

pub fn load(path: impl AsRef<Path>) -> anyhow::Result<AppConfig> {
    let path = path.as_ref();
    let config_str = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut config: AppConfig = toml::from_str(&config_str)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    override_config_from_env(&mut config);
    validate_llm_config(&mut config);
    validate_telegram_config(&mut config);
    validate_heartbeat_config(&mut config);
    Ok(config)
}

fn validate_llm_config(config: &mut AppConfig) {
    const MIN_CHAT_CONTEXT_MESSAGES: usize = 20;
    const MIN_EVENT_CONTEXT_MESSAGES: usize = 8;
    const MIN_CONTEXT_CHARS: usize = 20_000;
    const MIN_RECENT_DIALOGUE_MESSAGES: usize = 4;
    const MIN_ATTACHMENT_BYTES: usize = 1024 * 1024;
    const MAX_ATTACHMENT_BYTES: usize = 50_000_000;

    if config.llm.max_context_messages < MIN_CHAT_CONTEXT_MESSAGES {
        tracing::warn!(
            "llm.max_context_messages={} is too low; clamping to {}",
            config.llm.max_context_messages,
            MIN_CHAT_CONTEXT_MESSAGES
        );
        config.llm.max_context_messages = MIN_CHAT_CONTEXT_MESSAGES;
    }

    if config.llm.event_max_context_messages < MIN_EVENT_CONTEXT_MESSAGES {
        tracing::warn!(
            "llm.event_max_context_messages={} is too low; clamping to {}",
            config.llm.event_max_context_messages,
            MIN_EVENT_CONTEXT_MESSAGES
        );
        config.llm.event_max_context_messages = MIN_EVENT_CONTEXT_MESSAGES;
    }

    if config.llm.max_context_chars < MIN_CONTEXT_CHARS {
        tracing::warn!(
            "llm.max_context_chars={} is too low; clamping to {}",
            config.llm.max_context_chars,
            MIN_CONTEXT_CHARS
        );
        config.llm.max_context_chars = MIN_CONTEXT_CHARS;
    }
    if config.llm.context_compact_target_chars >= config.llm.max_context_chars {
        config.llm.context_compact_target_chars = config.llm.max_context_chars * 2 / 3;
        tracing::warn!(
            "llm.context_compact_target_chars must be below max_context_chars; using {}",
            config.llm.context_compact_target_chars
        );
    }
    config.llm.context_keep_recent_dialogue_messages = config
        .llm
        .context_keep_recent_dialogue_messages
        .max(MIN_RECENT_DIALOGUE_MESSAGES);
    config.llm.interactive_run_budget_seconds =
        config.llm.interactive_run_budget_seconds.clamp(60, 86_400);
    config.llm.llm_request_timeout_seconds = config.llm.llm_request_timeout_seconds.clamp(10, 600);
    config.llm.tool_call_timeout_seconds = config.llm.tool_call_timeout_seconds.clamp(5, 600);
    config.llm.max_tool_result_chars = config
        .llm
        .max_tool_result_chars
        .clamp(2_000, 100_000)
        .min(config.llm.context_compact_target_chars);
    config.llm.max_tool_batch_chars = config.llm.max_tool_batch_chars.clamp(
        config.llm.max_tool_result_chars,
        config.llm.context_compact_target_chars,
    );
    config.llm.max_attachment_bytes_per_request = config
        .llm
        .max_attachment_bytes_per_request
        .clamp(MIN_ATTACHMENT_BYTES, MAX_ATTACHMENT_BYTES);

    if let Some(model) = &mut config.llm.transcription_model {
        let normalized = model.trim().to_string();
        if normalized.is_empty() {
            config.llm.transcription_model = None;
        } else {
            *model = normalized;
        }
    }

    if let Some(service_tier) = &mut config.llm.service_tier {
        let normalized = service_tier.trim().to_string();
        if normalized.is_empty() {
            config.llm.service_tier = None;
        } else if normalized != *service_tier {
            *service_tier = normalized;
        }
    }

    if let Some(reasoning_context) = &mut config.llm.reasoning_context {
        let normalized = reasoning_context.trim().to_string();
        if normalized.is_empty() {
            config.llm.reasoning_context = None;
        } else if normalized != *reasoning_context {
            *reasoning_context = normalized;
        }
    }
}

fn validate_telegram_config(config: &mut AppConfig) {
    config.telegram.max_download_bytes = config
        .telegram
        .max_download_bytes
        .clamp(1, jossie_core::config::DEFAULT_TELEGRAM_MAX_DOWNLOAD_BYTES);
    config.telegram.ffmpeg_path = config.telegram.ffmpeg_path.trim().to_string();
    if config.telegram.ffmpeg_path.is_empty() {
        config.telegram.ffmpeg_path = "ffmpeg".to_string();
    }
}

fn validate_heartbeat_config(config: &mut AppConfig) {
    const MIN_HEARTBEAT_INTERVAL_SECS: u64 = 900;

    if config.heartbeat.interval_seconds < MIN_HEARTBEAT_INTERVAL_SECS {
        tracing::warn!(
            "heartbeat.interval_seconds={} is too low; clamping to {}",
            config.heartbeat.interval_seconds,
            MIN_HEARTBEAT_INTERVAL_SECS
        );
        config.heartbeat.interval_seconds = MIN_HEARTBEAT_INTERVAL_SECS;
    }
}

fn override_config_from_env(config: &mut AppConfig) {
    use std::env;

    if let Ok(val) = env::var("JOSSIE_SERVER_AUTH_TOKEN") {
        config.server.auth_token = val;
    }
    if let Ok(val) = env::var("JOSSIE_SERVER_AUTH_PASSWORD_HASH") {
        config.server.auth_password_hash = val;
    }
    if let Ok(val) = env::var("JOSSIE_SERVER_PUBLIC_BASE_URL") {
        let trimmed = val.trim();
        config.server.public_base_url = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Ok(val) = env::var("JOSSIE_LLM_API_KEY") {
        config.llm.api_key = val;
    }
    if let Ok(val) = env::var("JOSSIE_LLM_SYSTEM_PROMPT") {
        config.llm.system_prompt = val;
    }
    if let Some(parsed) = env_bool("JOSSIE_LLM_ENABLE_WEB_SEARCH") {
        config.llm.enable_web_search = parsed;
    }
    if let Ok(val) = env::var("JOSSIE_LLM_SERVICE_TIER") {
        config.llm.service_tier = Some(val);
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_MAX_CONTEXT_MESSAGES") {
        config.llm.max_context_messages = parsed;
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_EVENT_MAX_CONTEXT_MESSAGES") {
        config.llm.event_max_context_messages = parsed;
    }
    if let Some(parsed) = env_bool("JOSSIE_LLM_OPENAI_OPTIMIZATIONS") {
        config.llm.openai_optimizations = parsed;
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_MAX_CONTEXT_CHARS") {
        config.llm.max_context_chars = parsed;
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_CONTEXT_COMPACT_TARGET_CHARS") {
        config.llm.context_compact_target_chars = parsed;
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_CONTEXT_KEEP_RECENT_DIALOGUE_MESSAGES") {
        config.llm.context_keep_recent_dialogue_messages = parsed;
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_INTERACTIVE_RUN_BUDGET_SECONDS") {
        config.llm.interactive_run_budget_seconds = parsed;
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_REQUEST_TIMEOUT_SECONDS") {
        config.llm.llm_request_timeout_seconds = parsed;
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_TOOL_CALL_TIMEOUT_SECONDS") {
        config.llm.tool_call_timeout_seconds = parsed;
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_MAX_TOOL_RESULT_CHARS") {
        config.llm.max_tool_result_chars = parsed;
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_MAX_TOOL_BATCH_CHARS") {
        config.llm.max_tool_batch_chars = parsed;
    }
    if let Ok(val) = env::var("JOSSIE_LLM_TRANSCRIPTION_MODEL") {
        config.llm.transcription_model = Some(val);
    }
    if let Some(parsed) = env_parse("JOSSIE_LLM_MAX_ATTACHMENT_BYTES_PER_REQUEST") {
        config.llm.max_attachment_bytes_per_request = parsed;
    }
    if let Ok(val) = env::var("JOSSIE_TELEGRAM_BOT_TOKEN") {
        config.telegram.bot_token = val.trim().to_string();
    }
    if let Some(parsed) = env_parse("JOSSIE_TELEGRAM_MAX_DOWNLOAD_BYTES") {
        config.telegram.max_download_bytes = parsed;
    }
    if let Ok(val) = env::var("JOSSIE_TELEGRAM_FFMPEG_PATH") {
        config.telegram.ffmpeg_path = val;
    }

    // Email
    if let Ok(val) = env::var("JOSSIE_EMAIL_USERNAME") {
        config.email.username = val;
    }
    if let Ok(val) = env::var("JOSSIE_EMAIL_PASSWORD") {
        config.email.password = val;
    }
    if let Ok(val) = env::var("JOSSIE_EMAIL_IMAP_HOST") {
        config.email.imap_host = val;
    }
    if let Ok(val) = env::var("JOSSIE_EMAIL_SMTP_HOST") {
        config.email.smtp_host = val;
    }

    // Google
    if let Ok(val) = env::var("JOSSIE_GOOGLE_CLIENT_ID") {
        config.google.client_id = val;
    }
    if let Ok(val) = env::var("JOSSIE_GOOGLE_CLIENT_SECRET") {
        config.google.client_secret = val;
    }
    if let Ok(val) = env::var("JOSSIE_GOOGLE_REFRESH_TOKEN") {
        config.google.refresh_token = val;
    }
}

fn parse_env_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn env_bool(name: &str) -> Option<bool> {
    parse_env_bool(&std::env::var(name).ok()?)
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_clamps_unsafe_runtime_limits() {
        let mut config: AppConfig = toml::from_str(include_str!("../config.sample.toml")).unwrap();
        config.llm.max_context_messages = 1;
        config.llm.event_max_context_messages = 1;
        config.llm.llm_request_timeout_seconds = 1;
        config.telegram.ffmpeg_path = "  ".to_string();
        config.heartbeat.interval_seconds = 1;

        validate_llm_config(&mut config);
        validate_telegram_config(&mut config);
        validate_heartbeat_config(&mut config);

        assert_eq!(config.llm.max_context_messages, 20);
        assert_eq!(config.llm.event_max_context_messages, 8);
        assert_eq!(config.llm.llm_request_timeout_seconds, 10);
        assert_eq!(config.telegram.ffmpeg_path, "ffmpeg");
        assert_eq!(config.heartbeat.interval_seconds, 900);
    }
}
