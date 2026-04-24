use jossie_core::integration::{Integration, ToolDefinition};
use jossie_db::Database;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use totp_rs::{Algorithm, Secret, TOTP};

pub struct MemoryIntegration {
    db: Arc<Database>,
}

impl MemoryIntegration {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

#[derive(Debug, Serialize)]
struct MemoryValue {
    key: String,
    content: String,
    tags: String,
}

#[derive(Debug, Serialize)]
struct TotpResponse {
    key: String,
    field: Option<String>,
    code: String,
    generated_at: u64,
    valid_for_seconds: u64,
    period: u64,
    digits: usize,
    algorithm: String,
}

#[derive(Debug, Clone)]
struct ResolvedTotp {
    totp: TOTP,
    period: u64,
    digits: usize,
    algorithm: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredCredentialBundle {
    #[serde(default)]
    totp_secret: Option<String>,
    #[serde(default)]
    otp_secret: Option<String>,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    otpauth_url: Option<String>,
    #[serde(default)]
    otpauth: Option<String>,
    #[serde(default)]
    totp_algorithm: Option<String>,
    #[serde(default)]
    algorithm: Option<String>,
    #[serde(default)]
    totp_digits: Option<usize>,
    #[serde(default)]
    digits: Option<usize>,
    #[serde(default)]
    totp_period: Option<u64>,
    #[serde(default)]
    period: Option<u64>,
}

fn parse_totp_algorithm(value: Option<&str>) -> anyhow::Result<Algorithm> {
    match value
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("SHA1")
        .to_ascii_uppercase()
        .as_str()
    {
        "SHA1" | "SHA-1" => Ok(Algorithm::SHA1),
        "SHA256" | "SHA-256" => Ok(Algorithm::SHA256),
        "SHA512" | "SHA-512" => Ok(Algorithm::SHA512),
        other => anyhow::bail!("Unsupported TOTP algorithm '{other}'"),
    }
}

fn resolve_string_candidate(
    bundle: &StoredCredentialBundle,
    preferred_field: Option<&str>,
) -> Option<(String, Option<String>)> {
    let candidate = match preferred_field {
        Some("otpauth_url") | Some("otpauth") => bundle
            .otpauth_url
            .as_ref()
            .or(bundle.otpauth.as_ref())
            .map(|value| ("otpauth_url".to_string(), value.clone())),
        Some("totp_secret") | Some("otp_secret") | Some("secret") => bundle
            .totp_secret
            .as_ref()
            .or(bundle.otp_secret.as_ref())
            .or(bundle.secret.as_ref())
            .map(|value| ("totp_secret".to_string(), value.clone())),
        Some(other) => None.or_else(|| {
            let dynamic = serde_json::to_value(bundle).ok()?;
            dynamic
                .get(other)
                .and_then(|value| value.as_str())
                .map(|value| (other.to_string(), value.to_string()))
        }),
        None => bundle
            .otpauth_url
            .as_ref()
            .or(bundle.otpauth.as_ref())
            .map(|value| ("otpauth_url".to_string(), value.clone()))
            .or_else(|| {
                bundle
                    .totp_secret
                    .as_ref()
                    .or(bundle.otp_secret.as_ref())
                    .or(bundle.secret.as_ref())
                    .map(|value| ("totp_secret".to_string(), value.clone()))
            }),
    }?;

    Some((candidate.1, Some(candidate.0)))
}

fn resolve_totp(content: &str, preferred_field: Option<&str>) -> anyhow::Result<ResolvedTotp> {
    let trimmed = content.trim();

    if trimmed.starts_with("otpauth://") {
        let totp = TOTP::from_url(trimmed)?;
        return Ok(ResolvedTotp {
            period: totp.step,
            digits: totp.digits,
            algorithm: format!("{:?}", totp.algorithm),
            totp,
        });
    }

    let (secret_value, field_name, algorithm, digits, period) =
        if let Ok(bundle) = serde_json::from_str::<StoredCredentialBundle>(trimmed) {
            let (secret_value, field_name) = resolve_string_candidate(&bundle, preferred_field)
                .ok_or_else(|| anyhow::anyhow!("No TOTP secret found in stored JSON memory"))?;

            let algorithm = parse_totp_algorithm(
                bundle
                    .totp_algorithm
                    .as_deref()
                    .or(bundle.algorithm.as_deref()),
            )?;
            let digits = bundle.totp_digits.or(bundle.digits).unwrap_or(6);
            let period = bundle.totp_period.or(bundle.period).unwrap_or(30);
            (secret_value, field_name, algorithm, digits, period)
        } else {
            let algorithm = parse_totp_algorithm(None)?;
            let period = 30;
            (
                trimmed.to_string(),
                preferred_field.map(|field| field.to_string()),
                algorithm,
                6,
                period,
            )
        };

    let totp = if secret_value.starts_with("otpauth://") {
        TOTP::from_url(&secret_value)?
    } else {
        TOTP::new(
            algorithm,
            digits,
            1,
            period,
            Secret::Encoded(secret_value).to_bytes()?,
            None,
            String::new(),
        )?
    };

    Ok(ResolvedTotp {
        period: totp.step,
        digits: totp.digits,
        algorithm: format!("{:?}", totp.algorithm),
        totp: if field_name.as_deref() == Some("otpauth_url") {
            totp
        } else {
            totp
        },
    })
}

fn current_unix_timestamp() -> anyhow::Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[async_trait::async_trait]
impl Integration for MemoryIntegration {
    fn name(&self) -> &str {
        "memory"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "memory_save".to_string(),
                description: "Save durable context to long-term memory, such as preferences, relationships, ongoing projects, recurring needs, credentials, API keys, tokens, MFA seed material, or other information likely to matter again. For structured secrets, prefer stable keys such as `credential.rwth_sso` and JSON content. Use prompt_scope only for compact, non-secret memories that should be automatically included in future chat/event prompts."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "key": {"type": "string", "description": "Unique key for this memory"},
                        "content": {"type": "string", "description": "Durable content to remember"},
                        "tags": {"type": "string", "description": "Space-separated tags for categorization (use empty string for none)"},
                        "allow_sensitive": {"type": "boolean", "description": "Legacy compatibility flag. It is optional and ignored."},
                        "prompt_scope": {"type": "string", "enum": ["none", "chat", "event", "both"], "description": "Optional automatic prompt inclusion scope. Use none for secrets or low-signal memories."},
                        "importance": {"type": "integer", "minimum": 0, "maximum": 100, "description": "Optional prompt priority for prompt-scoped memories. Higher is included first."}
                    },
                    "required": ["key", "content", "tags"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "memory_get".to_string(),
                description: "Fetch one memory entry by its exact key. Use this for precise recall of stored credentials or other structured data instead of broad search."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "key": {"type": "string", "description": "Exact memory key to fetch"}
                    },
                    "required": ["key"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "memory_generate_totp".to_string(),
                description: "Generate the current TOTP code from a stored memory entry. The memory content can be a raw Base32 secret, an `otpauth://` URL, or structured JSON with fields such as `totp_secret`, `otpauth_url`, `totp_algorithm`, `totp_digits`, and `totp_period`."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "key": {"type": "string", "description": "Exact memory key that stores the TOTP secret or otpauth URL"},
                        "field": {"type": ["string", "null"], "description": "Optional JSON field to use when the memory content is structured. Defaults to `otpauth_url` or `totp_secret` when present."},
                        "timestamp": {"type": ["integer", "null"], "description": "Optional Unix timestamp in seconds for deterministic generation. Defaults to the current time."}
                    },
                    "required": ["key"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "memory_search".to_string(),
                description: "Search long-term memory using a few focused keywords. Prefer 2-6 specific terms, names, project titles, dates, or tags. If needed, run multiple narrower searches instead of one long laundry-list query."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query"}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "memory_list_keys".to_string(),
                description: "List all keys stored in long-term memory with timestamps".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "memory_list_all".to_string(),
                description: "List all memories with full content".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "limit": {"type": "integer", "description": "Number of memories to return (default 50, max 500)"}
                    },
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        match tool_name {
            "memory_save" => {
                #[derive(Deserialize)]
                struct Args {
                    key: String,
                    content: String,
                    #[serde(default)]
                    tags: String,
                    #[serde(default)]
                    allow_sensitive: bool,
                    #[serde(default)]
                    prompt_scope: Option<String>,
                    #[serde(default)]
                    importance: Option<i64>,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let _ = args.allow_sensitive;
                self.db
                    .memory_save_with_prompt_metadata(
                        &args.key,
                        &args.content,
                        &args.tags,
                        args.prompt_scope.as_deref(),
                        args.importance,
                    )
                    .await?;
                Ok(format!("Saved memory with key '{}'", args.key))
            }
            "memory_get" => {
                #[derive(Deserialize)]
                struct Args {
                    key: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let entry = self
                    .db
                    .get_memory(&args.key)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("No memory found for key '{}'", args.key))?;
                Ok(serde_json::to_string_pretty(&MemoryValue {
                    key: entry.key,
                    content: entry.content,
                    tags: entry.tags,
                })?)
            }
            "memory_generate_totp" => {
                #[derive(Deserialize)]
                struct Args {
                    key: String,
                    #[serde(default)]
                    field: Option<String>,
                    #[serde(default)]
                    timestamp: Option<u64>,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let entry = self
                    .db
                    .get_memory(&args.key)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("No memory found for key '{}'", args.key))?;
                let resolved = resolve_totp(&entry.content, args.field.as_deref())?;
                let generated_at = match args.timestamp {
                    Some(timestamp) => timestamp,
                    None => current_unix_timestamp()?,
                };
                let code = resolved.totp.generate(generated_at);
                let remainder = generated_at % resolved.period;
                let valid_for_seconds = if remainder == 0 {
                    resolved.period
                } else {
                    resolved.period - remainder
                };
                Ok(serde_json::to_string_pretty(&TotpResponse {
                    key: args.key,
                    field: args.field,
                    code,
                    generated_at,
                    valid_for_seconds,
                    period: resolved.period,
                    digits: resolved.digits,
                    algorithm: resolved.algorithm,
                })?)
            }
            "memory_search" => {
                #[derive(Deserialize)]
                struct Args {
                    query: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let results = self.db.memory_search(&args.query).await?;
                Ok(serde_json::to_string_pretty(&results)?)
            }
            "memory_list_keys" => {
                let results = self.db.memory_list_keys().await?;
                Ok(serde_json::to_string_pretty(&results)?)
            }
            "memory_list_all" => {
                #[derive(Deserialize)]
                struct Args {
                    #[serde(default = "default_limit")]
                    limit: usize,
                }
                fn default_limit() -> usize {
                    50
                }
                let args: Args = serde_json::from_str(arguments).unwrap_or(Args { limit: 50 });
                let results = self.db.memory_list_all(args.limit).await?;
                Ok(serde_json::to_string_pretty(&results)?)
            }
            _ => anyhow::bail!("Unknown tool: {tool_name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jossie_core::integration::Integration;
    use jossie_db::Database;

    async fn test_memory() -> MemoryIntegration {
        let db = Database::new("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        MemoryIntegration::new(Arc::new(db))
    }

    #[tokio::test]
    async fn tools_are_defined() {
        let mem = test_memory().await;
        let tools = mem.tools();
        assert_eq!(tools.len(), 6);
        assert!(tools.iter().any(|t| t.name == "memory_save"));
        assert!(tools.iter().any(|t| t.name == "memory_get"));
        assert!(tools.iter().any(|t| t.name == "memory_generate_totp"));
        assert!(tools.iter().any(|t| t.name == "memory_search"));
        assert!(tools.iter().any(|t| t.name == "memory_list_keys"));
        assert!(tools.iter().any(|t| t.name == "memory_list_all"));
    }

    #[tokio::test]
    async fn save_get_and_search() {
        let mem = test_memory().await;

        let save_result = mem
            .execute(
                "memory_save",
                r#"{"key":"test","content":"important info","tags":"test"}"#,
            )
            .await
            .unwrap();
        assert!(save_result.contains("Saved"));

        let get_result = mem
            .execute("memory_get", r#"{"key":"test"}"#)
            .await
            .unwrap();
        assert!(get_result.contains("\"content\": \"important info\""));

        let search_result = mem
            .execute("memory_search", r#"{"query":"important"}"#)
            .await
            .unwrap();
        assert!(search_result.contains("important info"));
    }

    #[tokio::test]
    async fn saves_sensitive_memory_without_override() {
        let mem = test_memory().await;

        let result = mem
            .execute(
                "memory_save",
                r#"{"key":"ops","content":"password: hunter2","tags":""}"#,
            )
            .await
            .unwrap();

        assert!(result.contains("Saved"));
    }

    #[tokio::test]
    async fn generates_totp_from_structured_json_memory() {
        let mem = test_memory().await;

        mem.execute(
            "memory_save",
            r#"{"key":"credential.rwth_sso","content":"{\"username\":\"ab123456\",\"totp_secret\":\"GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ\",\"totp_digits\":8,\"totp_algorithm\":\"SHA1\",\"totp_period\":30}","tags":"credential rwth"}"#,
        )
        .await
        .unwrap();

        let result = mem
            .execute(
                "memory_generate_totp",
                r#"{"key":"credential.rwth_sso","timestamp":59}"#,
            )
            .await
            .unwrap();

        assert!(result.contains("\"code\": \"94287082\""));
        assert!(result.contains("\"digits\": 8"));
        assert!(result.contains("\"period\": 30"));
    }

    #[tokio::test]
    async fn generates_totp_from_otpauth_url() {
        let mem = test_memory().await;

        mem.execute(
            "memory_save",
            r#"{"key":"credential.rwth_sso","content":"otpauth://totp/RWTH:ab123456?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=RWTH&algorithm=SHA1&digits=8&period=30","tags":"credential rwth"}"#,
        )
        .await
        .unwrap();

        let result = mem
            .execute(
                "memory_generate_totp",
                r#"{"key":"credential.rwth_sso","timestamp":59}"#,
            )
            .await
            .unwrap();

        assert!(result.contains("\"code\": \"94287082\""));
    }

    #[tokio::test]
    async fn list_keys_and_all() {
        let mem = test_memory().await;

        mem.execute("memory_save", r#"{"key":"k1","content":"c1","tags":"t1"}"#)
            .await
            .unwrap();
        mem.execute("memory_save", r#"{"key":"k2","content":"c2","tags":"t2"}"#)
            .await
            .unwrap();

        let keys_json = mem.execute("memory_list_keys", "{}").await.unwrap();
        let keys: Vec<serde_json::Value> = serde_json::from_str(&keys_json).unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().any(|k| k["key"] == "k1"));
        assert!(keys.iter().any(|k| k["key"] == "k2"));

        let all_json = mem.execute("memory_list_all", "{}").await.unwrap();
        let all: Vec<serde_json::Value> = serde_json::from_str(&all_json).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|e| e["key"] == "k1" && e["content"] == "c1"));
        assert!(all.iter().any(|e| e["key"] == "k2" && e["content"] == "c2"));
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let mem = test_memory().await;
        let result = mem.execute("nonexistent", "{}").await;
        assert!(result.is_err());
    }
}
