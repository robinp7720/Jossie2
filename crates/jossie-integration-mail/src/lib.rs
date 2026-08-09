use anyhow::{Context, anyhow};
use jossie_core::integration::{Integration, ToolDefinition};
use jossie_integration_email::{EmailIntegration, EmailSearchRequest};
use jossie_integration_google::GoogleIntegration;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

const IMAP_PROVIDER: &str = "imap";
const GMAIL_PROVIDER: &str = "gmail";
const MAX_UNIFIED_MAIL_BODY_CHARS: usize = 12_000;

pub struct MailIntegration {
    email: Arc<EmailIntegration>,
    google: Option<Arc<GoogleIntegration>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MailProvider {
    Imap,
    Gmail,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct MessageRef {
    provider: String,
    account_id: String,
    external_id: String,
    #[serde(default)]
    mailbox: Option<String>,
    #[serde(default)]
    native: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct MailSearchArgs {
    account_id: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    terms: Vec<String>,
    #[serde(default = "default_match_mode")]
    r#match: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    mailbox: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
    #[serde(default)]
    page_token: Option<String>,
}

fn default_match_mode() -> String {
    "any".to_string()
}

#[derive(Debug, Deserialize)]
struct MailReadArgs {
    message_ref: MessageRef,
}

#[derive(Debug, Deserialize)]
struct MailSendArgs {
    account_id: String,
    to: String,
    subject: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct MailboxArgs {
    account_id: String,
}

impl MailIntegration {
    pub fn new(email: Arc<EmailIntegration>, google: Option<Arc<GoogleIntegration>>) -> Self {
        Self { email, google }
    }

    fn unified_account_id(provider: MailProvider, provider_account_id: &str) -> String {
        let prefix = match provider {
            MailProvider::Imap => IMAP_PROVIDER,
            MailProvider::Gmail => GMAIL_PROVIDER,
        };
        format!("{prefix}:{provider_account_id}")
    }

    fn split_account_id(account_id: &str) -> anyhow::Result<(MailProvider, &str)> {
        let (provider, provider_account_id) = account_id
            .split_once(':')
            .ok_or_else(|| anyhow!("Unsupported mail account ID: {account_id}"))?;
        let provider = match provider {
            IMAP_PROVIDER => MailProvider::Imap,
            GMAIL_PROVIDER => MailProvider::Gmail,
            _ => anyhow::bail!("Unknown mail provider prefix: {provider}"),
        };
        if provider_account_id.trim().is_empty() {
            anyhow::bail!("Missing provider account ID in {account_id}");
        }
        Ok((provider, provider_account_id))
    }

    fn parse_json_value(input: &str, context: &str) -> anyhow::Result<Value> {
        serde_json::from_str::<Value>(input)
            .with_context(|| format!("{context} returned invalid JSON"))
    }

    async fn list_imap_accounts(&self) -> anyhow::Result<Vec<Value>> {
        let accounts = self.email.mail_accounts().await?;

        Ok(accounts
            .into_iter()
            .map(|account| {
                let provider_account_id = account
                    .get("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                json!({
                    "id": Self::unified_account_id(MailProvider::Imap, provider_account_id),
                    "provider": IMAP_PROVIDER,
                    "provider_account_id": provider_account_id,
                    "name": account.get("name").and_then(|value| value.as_str()).unwrap_or_default(),
                    "email": account.get("email").and_then(|value| value.as_str()).unwrap_or_default(),
                    "capabilities": ["search", "read", "send", "list_mailboxes"],
                })
            })
            .collect())
    }

    async fn list_gmail_accounts(&self) -> anyhow::Result<Vec<Value>> {
        let Some(google) = &self.google else {
            return Ok(Vec::new());
        };

        let raw = google.list_accounts().await?;
        let accounts = Self::parse_json_value(&raw, "google_list_accounts")?
            .as_array()
            .cloned()
            .unwrap_or_default();

        Ok(accounts
            .into_iter()
            .map(|account| {
                let provider_account_id = account
                    .get("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                json!({
                    "id": Self::unified_account_id(MailProvider::Gmail, provider_account_id),
                    "provider": GMAIL_PROVIDER,
                    "provider_account_id": provider_account_id,
                    "name": account.get("name").and_then(|value| value.as_str()).unwrap_or_default(),
                    "email": account.get("email").and_then(|value| value.as_str()).unwrap_or_default(),
                    "capabilities": ["search", "read", "send", "list_mailboxes"],
                })
            })
            .collect())
    }

    async fn mail_list_accounts(&self) -> anyhow::Result<String> {
        let mut accounts = self.list_imap_accounts().await?;
        accounts.extend(self.list_gmail_accounts().await?);
        Ok(serde_json::to_string_pretty(&accounts)?)
    }

    async fn mail_search(&self, args: MailSearchArgs) -> anyhow::Result<String> {
        let account_id = args.account_id.clone();
        let (provider, provider_account_id) = Self::split_account_id(&account_id)?;
        match provider {
            MailProvider::Imap => self.mail_search_imap(provider_account_id, args).await,
            MailProvider::Gmail => self.mail_search_gmail(provider_account_id, args).await,
        }
    }

    async fn mail_search_imap(
        &self,
        provider_account_id: &str,
        args: MailSearchArgs,
    ) -> anyhow::Result<String> {
        let payload = self
            .email
            .mail_search(
                provider_account_id,
                EmailSearchRequest {
                    query: args.query.clone(),
                    terms: args.terms.clone(),
                    match_mode: args.r#match.clone(),
                    from: args.from.clone(),
                    subject: args.subject.clone(),
                    after: args.after.clone(),
                    before: args.before.clone(),
                    max_results: args.max_results,
                    page_token: args.page_token.clone(),
                    folder: args.mailbox.clone(),
                },
            )
            .await?;
        let messages = payload
            .get("messages")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();

        let mailbox = normalize_optional_mailbox(args.mailbox.as_deref());
        let normalized: Vec<Value> = messages
            .into_iter()
            .map(|message| {
                let uid = message
                    .get("uid")
                    .and_then(|value| value.as_u64())
                    .unwrap_or_default();
                let uid_str = uid.to_string();
                json!({
                    "message_ref": Self::build_imap_message_ref(provider_account_id, &uid_str, mailbox.as_deref()),
                    "from": message.get("from").and_then(|value| value.as_str()).unwrap_or_default(),
                    "to": Vec::<String>::new(),
                    "subject": message.get("subject").and_then(|value| value.as_str()).unwrap_or_default(),
                    "date": message.get("date").and_then(|value| value.as_str()).unwrap_or_default(),
                    "snippet": "",
                    "mailbox": mailbox.clone().unwrap_or_else(|| "INBOX".to_string()),
                })
            })
            .collect();

        Ok(serde_json::to_string_pretty(&json!({
            "messages": normalized,
            "next_page_token": payload.get("next_page_token").cloned().unwrap_or(Value::Null),
        }))?)
    }

    async fn mail_search_gmail(
        &self,
        provider_account_id: &str,
        args: MailSearchArgs,
    ) -> anyhow::Result<String> {
        let google = self
            .google
            .as_ref()
            .ok_or_else(|| anyhow!("Google integration is not configured"))?;
        let query = build_gmail_search_query(&args);
        let raw = google
            .mail_search(
                provider_account_id,
                &query,
                args.max_results,
                args.page_token.as_deref(),
            )
            .await?;
        let payload = Self::parse_json_value(&raw, "Google mail search")?;
        let messages = payload
            .get("messages")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();

        let normalized: Vec<Value> = messages
            .into_iter()
            .map(|message| {
                let headers = message
                    .get("headers")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mailbox = normalize_optional_mailbox(args.mailbox.as_deref());
                let external_id = message
                    .get("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                json!({
                    "message_ref": Self::build_gmail_message_ref(provider_account_id, external_id, mailbox.as_deref()),
                    "from": header_value(&headers, "From"),
                    "to": split_recipients(&header_value(&headers, "To")),
                    "subject": header_value(&headers, "Subject"),
                    "date": header_value(&headers, "Date"),
                    "snippet": message.get("snippet").and_then(|value| value.as_str()).unwrap_or_default(),
                    "mailbox": mailbox,
                })
            })
            .collect();

        Ok(serde_json::to_string_pretty(&json!({
            "messages": normalized,
            "next_page_token": payload.get("next_page_token").cloned().unwrap_or(Value::Null),
        }))?)
    }

    async fn mail_read(&self, args: MailReadArgs) -> anyhow::Result<String> {
        let account_id = args.message_ref.account_id.clone();
        let (provider, provider_account_id) = Self::split_account_id(&account_id)?;
        match provider {
            MailProvider::Imap => {
                self.mail_read_imap(provider_account_id, args.message_ref)
                    .await
            }
            MailProvider::Gmail => {
                self.mail_read_gmail(provider_account_id, args.message_ref)
                    .await
            }
        }
    }

    async fn mail_read_imap(
        &self,
        provider_account_id: &str,
        message_ref: MessageRef,
    ) -> anyhow::Result<String> {
        let payload = self
            .email
            .mail_read(
                provider_account_id,
                parse_imap_uid(&message_ref.external_id)?,
                message_ref.mailbox.as_deref(),
            )
            .await?;
        Ok(serde_json::to_string_pretty(&json!({
            "message_ref": message_ref,
            "from": payload.get("from").and_then(|value| value.as_str()).unwrap_or_default(),
            "to": payload.get("to").and_then(|value| value.as_array()).cloned().unwrap_or_default(),
            "subject": payload.get("subject").and_then(|value| value.as_str()).unwrap_or_default(),
            "date": payload.get("date").and_then(|value| value.as_str()).unwrap_or_default(),
            "body": compact_mail_body(payload.get("body").and_then(|value| value.as_str()).unwrap_or_default()),
            "attachments": Vec::<Value>::new(),
            "mailbox": message_ref.mailbox.clone().unwrap_or_else(|| "INBOX".to_string()),
        }))?)
    }

    async fn mail_read_gmail(
        &self,
        provider_account_id: &str,
        message_ref: MessageRef,
    ) -> anyhow::Result<String> {
        let google = self
            .google
            .as_ref()
            .ok_or_else(|| anyhow!("Google integration is not configured"))?;
        let raw = google
            .mail_read(provider_account_id, &message_ref.external_id)
            .await?;
        let payload = Self::parse_json_value(&raw, "Google mail read")?;
        Ok(serde_json::to_string_pretty(&json!({
            "message_ref": message_ref,
            "from": payload.get("from").and_then(|value| value.as_str()).unwrap_or_default(),
            "to": split_recipients(payload.get("to").and_then(|value| value.as_str()).unwrap_or_default()),
            "subject": payload.get("subject").and_then(|value| value.as_str()).unwrap_or_default(),
            "date": payload.get("date").and_then(|value| value.as_str()).unwrap_or_default(),
            "body": compact_mail_body(payload.get("body").and_then(|value| value.as_str()).unwrap_or_default()),
            "attachments": payload.get("attachments").cloned().unwrap_or_else(|| json!([])),
            "mailbox": message_ref.mailbox,
        }))?)
    }

    async fn mail_send(&self, args: MailSendArgs) -> anyhow::Result<String> {
        let (provider, provider_account_id) = Self::split_account_id(&args.account_id)?;
        let result = match provider {
            MailProvider::Imap => {
                self.email
                    .mail_send(provider_account_id, &args.to, &args.subject, &args.body)
                    .await?
            }
            MailProvider::Gmail => {
                let google = self
                    .google
                    .as_ref()
                    .ok_or_else(|| anyhow!("Google integration is not configured"))?;
                google
                    .mail_send(provider_account_id, &args.to, &args.subject, &args.body)
                    .await?
            }
        };

        Ok(serde_json::to_string_pretty(&json!({
            "status": "sent",
            "provider": provider_name(provider),
            "account_id": args.account_id,
            "result": result,
        }))?)
    }

    async fn mail_list_mailboxes(&self, args: MailboxArgs) -> anyhow::Result<String> {
        let (provider, provider_account_id) = Self::split_account_id(&args.account_id)?;
        let mailboxes = match provider {
            MailProvider::Imap => {
                let folders = self.email.mail_folders(provider_account_id).await?;
                folders
                    .into_iter()
                    .map(|name| {
                        json!({
                            "name": name,
                            "display_name": name,
                            "kind": "folder",
                        })
                    })
                    .collect::<Vec<_>>()
            }
            MailProvider::Gmail => {
                let google = self
                    .google
                    .as_ref()
                    .ok_or_else(|| anyhow!("Google integration is not configured"))?;
                let raw = google.mail_labels(provider_account_id).await?;
                let labels = Self::parse_json_value(&raw, "Google mail labels")?
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                labels
                    .into_iter()
                    .map(|label| {
                        json!({
                            "name": label.get("name").and_then(|value| value.as_str()).unwrap_or_default(),
                            "display_name": label.get("name").and_then(|value| value.as_str()).unwrap_or_default(),
                            "kind": label.get("type").and_then(|value| value.as_str()).unwrap_or("label").to_ascii_lowercase(),
                            "id": label.get("id").and_then(|value| value.as_str()).unwrap_or_default(),
                        })
                    })
                    .collect::<Vec<_>>()
            }
        };

        Ok(serde_json::to_string_pretty(&mailboxes)?)
    }

    fn build_imap_message_ref(
        provider_account_id: &str,
        external_id: &str,
        mailbox: Option<&str>,
    ) -> MessageRef {
        MessageRef {
            provider: IMAP_PROVIDER.to_string(),
            account_id: Self::unified_account_id(MailProvider::Imap, provider_account_id),
            external_id: external_id.to_string(),
            mailbox: mailbox.map(|value| value.to_string()),
            native: Some(json!({
                "uid": external_id.parse::<u32>().ok(),
            })),
        }
    }

    fn build_gmail_message_ref(
        provider_account_id: &str,
        external_id: &str,
        mailbox: Option<&str>,
    ) -> MessageRef {
        MessageRef {
            provider: GMAIL_PROVIDER.to_string(),
            account_id: Self::unified_account_id(MailProvider::Gmail, provider_account_id),
            external_id: external_id.to_string(),
            mailbox: mailbox.map(|value| value.to_string()),
            native: None,
        }
    }
}

fn compact_mail_body(body: &str) -> String {
    let text = if body.contains('<') && body.contains('>') {
        jossie_core::text::html_to_text(body)
    } else {
        body.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    jossie_core::text::truncate_with_notice(text, MAX_UNIFIED_MAIL_BODY_CHARS)
}

#[async_trait::async_trait]
impl Integration for MailIntegration {
    fn name(&self) -> &str {
        "mail"
    }

    fn show_in_onboarding(&self) -> bool {
        false
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "mail_list_accounts".to_string(),
                description:
                    "List all configured mail accounts across IMAP/SMTP and Gmail providers."
                        .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "mail_search".to_string(),
                description:
                    "Search email consistently across Gmail and IMAP. Prefer one structured search with several terms over overlapping provider-specific queries; paginate with next_page_token when completeness matters."
                        .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Unified mail account ID from mail_list_accounts"},
                        "terms": {"type": "array", "items": {"type": "string"}, "description": "Plain-text terms to match"},
                        "match": {"type": "string", "enum": ["any", "all"], "description": "Whether any or all terms must match"},
                        "from": {"type": ["string", "null"], "description": "Optional sender filter"},
                        "subject": {"type": ["string", "null"], "description": "Optional subject filter"},
                        "after": {"type": ["string", "null"], "description": "Optional inclusive date in YYYY-MM-DD format"},
                        "before": {"type": ["string", "null"], "description": "Optional exclusive date in YYYY-MM-DD format"},
                        "mailbox": {"type": ["string", "null"], "description": "Optional mailbox, folder, or Gmail label filter"},
                        "max_results": {"type": ["integer", "null"], "description": "Optional provider-specific page size hint"},
                        "page_token": {"type": ["string", "null"], "description": "Optional pagination token for providers that support it"}
                    },
                    "required": ["account_id", "terms", "match", "from", "subject", "after", "before", "mailbox", "max_results", "page_token"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "mail_read".to_string(),
                description: "Read a specific email using the message_ref returned by mail_search."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "message_ref": {
                            "type": "object",
                            "properties": {
                                "provider": {"type": "string"},
                                "account_id": {"type": "string"},
                                "external_id": {"type": "string"},
                                "mailbox": {"type": ["string", "null"]},
                                "native": {}
                            },
                            "required": ["provider", "account_id", "external_id", "mailbox", "native"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["message_ref"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "mail_send".to_string(),
                description: "Send an email through the selected unified mail account.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Unified mail account ID from mail_list_accounts"},
                        "to": {"type": "string", "description": "Recipient email address"},
                        "subject": {"type": "string", "description": "Email subject"},
                        "body": {"type": "string", "description": "Email body text"}
                    },
                    "required": ["account_id", "to", "subject", "body"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "mail_list_mailboxes".to_string(),
                description:
                    "List folders, mailboxes, or labels for the selected unified mail account."
                        .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Unified mail account ID from mail_list_accounts"}
                    },
                    "required": ["account_id"],
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        match tool_name {
            "mail_list_accounts" => self.mail_list_accounts().await,
            "mail_search" => self.mail_search(serde_json::from_str(arguments)?).await,
            "mail_read" => self.mail_read(serde_json::from_str(arguments)?).await,
            "mail_send" => self.mail_send(serde_json::from_str(arguments)?).await,
            "mail_list_mailboxes" => {
                self.mail_list_mailboxes(serde_json::from_str(arguments)?)
                    .await
            }
            _ => anyhow::bail!("Unknown mail tool: {tool_name}"),
        }
    }
}

fn provider_name(provider: MailProvider) -> &'static str {
    match provider {
        MailProvider::Imap => IMAP_PROVIDER,
        MailProvider::Gmail => GMAIL_PROVIDER,
    }
}

fn parse_imap_uid(external_id: &str) -> anyhow::Result<u32> {
    external_id
        .parse::<u32>()
        .with_context(|| format!("Invalid IMAP UID: {external_id}"))
}

fn normalize_optional_mailbox(mailbox: Option<&str>) -> Option<String> {
    mailbox
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn merge_gmail_query(mailbox: Option<&str>, query: &str) -> String {
    let mailbox_term = normalize_optional_mailbox(mailbox).map(|value| format!("in:{value}"));
    let trimmed_query = query.trim();
    match (mailbox_term, trimmed_query.is_empty()) {
        (Some(mailbox_term), false) => format!("{mailbox_term} {trimmed_query}"),
        (Some(mailbox_term), true) => mailbox_term,
        (None, _) => trimmed_query.to_string(),
    }
}

fn build_gmail_search_query(args: &MailSearchArgs) -> String {
    let mut parts = Vec::new();
    if !args.terms.is_empty() {
        let terms = args
            .terms
            .iter()
            .map(|term| term.trim())
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        if !terms.is_empty() {
            parts.push(if args.r#match == "all" {
                terms.join(" ")
            } else {
                format!("({})", terms.join(" OR "))
            });
        }
    }
    if let Some(query) = args
        .query
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(query.trim().to_string());
    }
    if let Some(from) = args
        .from
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(format!("from:{}", from.trim()));
    }
    if let Some(subject) = args
        .subject
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(format!("subject:{}", subject.trim()));
    }
    if let Some(after) = args
        .after
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(format!("after:{}", after.replace('-', "/")));
    }
    if let Some(before) = args
        .before
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(format!("before:{}", before.replace('-', "/")));
    }
    merge_gmail_query(args.mailbox.as_deref(), &parts.join(" "))
}

fn split_recipients(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect()
}

fn header_value(headers: &[Value], name: &str) -> String {
    headers
        .iter()
        .find(|header| {
            header
                .get("name")
                .and_then(|value| value.as_str())
                .map(|header_name| header_name.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        })
        .and_then(|header| header.get("value"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        MailIntegration, MailProvider, MailSearchArgs, MessageRef, build_gmail_search_query,
        compact_mail_body, header_value, merge_gmail_query, split_recipients,
    };
    use serde_json::json;
    use std::sync::Arc;

    use jossie_core::{config::EmailConfig, integration::Integration};
    use jossie_integration_email::EmailIntegration;

    #[test]
    fn builds_prefixed_unified_account_ids() {
        assert_eq!(
            MailIntegration::unified_account_id(MailProvider::Imap, "default"),
            "imap:default"
        );
        assert_eq!(
            MailIntegration::unified_account_id(MailProvider::Gmail, "acc_1"),
            "gmail:acc_1"
        );
    }

    #[test]
    fn merges_gmail_mailbox_into_query() {
        assert_eq!(
            merge_gmail_query(Some("INBOX"), "from:alice"),
            "in:INBOX from:alice"
        );
        assert_eq!(merge_gmail_query(Some("STARRED"), ""), "in:STARRED");
        assert_eq!(merge_gmail_query(None, "project update"), "project update");
    }

    #[test]
    fn builds_structured_gmail_query() {
        let query = build_gmail_search_query(&MailSearchArgs {
            account_id: "gmail:test".to_string(),
            query: None,
            terms: vec!["receipt".to_string(), "invoice".to_string()],
            r#match: "any".to_string(),
            from: Some("shop@example.com".to_string()),
            subject: None,
            after: Some("2026-07-01".to_string()),
            before: Some("2026-08-01".to_string()),
            mailbox: Some("INBOX".to_string()),
            max_results: Some(20),
            page_token: None,
        });
        assert_eq!(
            query,
            "in:INBOX (receipt OR invoice) from:shop@example.com after:2026/07/01 before:2026/08/01"
        );
    }

    #[test]
    fn compacts_html_mail_body() {
        let body = compact_mail_body("<html><body><p>Paid <b>€7.50</b></p></body></html>");
        assert_eq!(body, "Paid €7.50");
        assert!(!body.contains('<'));
    }

    #[test]
    fn caps_unified_mail_body() {
        let body = compact_mail_body(&"x".repeat(20_000));
        assert!(body.starts_with(&"x".repeat(12_000)));
        assert!(body.contains("Message truncated"));
    }

    #[test]
    fn splits_recipient_lists() {
        assert_eq!(
            split_recipients("alice@example.com, Bob <bob@example.com>"),
            vec!["alice@example.com", "Bob <bob@example.com>"]
        );
    }

    #[test]
    fn extracts_header_value_case_insensitively() {
        let headers = vec![
            json!({"name": "from", "value": "alice@example.com"}),
            json!({"name": "Subject", "value": "Quarterly update"}),
        ];
        assert_eq!(header_value(&headers, "From"), "alice@example.com");
        assert_eq!(header_value(&headers, "subject"), "Quarterly update");
    }

    #[test]
    fn message_ref_round_trips() {
        let value = json!({
            "provider": "gmail",
            "account_id": "gmail:acc_1",
            "external_id": "msg_1",
            "mailbox": "INBOX",
            "native": {"thread_id": "thread_1"}
        });
        let parsed: MessageRef = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }

    #[test]
    fn unified_integration_is_the_only_public_mail_tool_surface() {
        let integration = MailIntegration::new(
            Arc::new(EmailIntegration::new(&EmailConfig::default())),
            None,
        );
        let names = integration
            .tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "mail_list_accounts",
                "mail_search",
                "mail_read",
                "mail_send",
                "mail_list_mailboxes",
            ]
        );
    }
}
