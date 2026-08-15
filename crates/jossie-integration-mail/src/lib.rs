use anyhow::{Context, anyhow};
use jossie_core::integration::{EmptyToolArgs, Integration, ToolDefinition};
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessageRef {
    pub provider: String,
    pub account_id: String,
    pub external_id: String,
    #[schemars(required)]
    pub mailbox: Option<String>,
    #[schemars(required)]
    pub native: Option<Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MailAttachmentRef {
    pub provider: String,
    pub attachment_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct MailMessageEvidence {
    pub message_ref: MessageRef,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub date: String,
    pub body: String,
    pub body_source: String,
    pub attachments: Vec<MailAttachmentRef>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailSearchArgs {
    /// Unified mail account ID from mail_list_accounts.
    account_id: String,
    /// Legacy single-query form. Prefer terms.
    #[serde(default)]
    #[schemars(skip)]
    query: Option<String>,
    /// Plain-text terms to match.
    #[schemars(required)]
    terms: Vec<String>,
    /// Whether any or all terms must match.
    #[schemars(required)]
    r#match: String,
    /// Optional sender filter.
    #[schemars(required)]
    from: Option<String>,
    /// Optional subject filter.
    #[schemars(required)]
    subject: Option<String>,
    /// Optional inclusive date in YYYY-MM-DD format.
    #[schemars(required)]
    after: Option<String>,
    /// Optional exclusive date in YYYY-MM-DD format.
    #[schemars(required)]
    before: Option<String>,
    /// Optional mailbox, folder, or Gmail label filter.
    #[schemars(required)]
    mailbox: Option<String>,
    /// Optional provider-specific page size hint.
    #[schemars(required)]
    max_results: Option<u32>,
    /// Optional pagination token for providers that support it.
    #[schemars(required)]
    page_token: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailReadArgs {
    message_ref: MessageRef,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailSendArgs {
    /// Unified mail account ID from mail_list_accounts.
    account_id: String,
    /// Recipient email address.
    to: String,
    /// Email subject.
    subject: String,
    /// Email body text.
    body: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailboxArgs {
    /// Unified mail account ID from mail_list_accounts.
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

        let accounts = google.account_values().await?;

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
        let payload = google
            .mail_search_value(
                provider_account_id,
                &query,
                args.max_results,
                args.page_token.as_deref(),
            )
            .await?;
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
        let evidence = self.read_message_evidence(args.message_ref).await?;
        let mailbox = evidence.message_ref.mailbox.clone();
        Ok(serde_json::to_string_pretty(&json!({
            "message_ref": evidence.message_ref,
            "from": evidence.from,
            "to": evidence.to,
            "subject": evidence.subject,
            "date": evidence.date,
            "body": evidence.body,
            "body_source": evidence.body_source,
            "attachments": evidence.attachments,
            "mailbox": mailbox,
        }))?)
    }

    pub async fn read_message_evidence(
        &self,
        message_ref: MessageRef,
    ) -> anyhow::Result<MailMessageEvidence> {
        let account_id = message_ref.account_id.clone();
        let (provider, provider_account_id) = Self::split_account_id(&account_id)?;
        match provider {
            MailProvider::Imap => {
                let content = self
                    .email
                    .mail_read_content(
                        provider_account_id,
                        parse_imap_uid(&message_ref.external_id)?,
                        message_ref.mailbox.as_deref(),
                    )
                    .await?;
                Ok(MailMessageEvidence {
                    message_ref,
                    from: content.from,
                    to: content.to,
                    subject: content.subject,
                    date: content.date,
                    body: compact_mail_body(&content.body),
                    body_source: content.body_source,
                    attachments: content
                        .attachments
                        .into_iter()
                        .map(|attachment| MailAttachmentRef {
                            provider: IMAP_PROVIDER.to_string(),
                            attachment_id: attachment.part_id,
                            filename: attachment.filename,
                            mime_type: attachment.mime_type,
                            size: attachment.size,
                        })
                        .collect(),
                })
            }
            MailProvider::Gmail => {
                let google = self
                    .google
                    .as_ref()
                    .ok_or_else(|| anyhow!("Google integration is not configured"))?;
                let payload = google
                    .mail_read_value(provider_account_id, &message_ref.external_id)
                    .await?;
                let attachments = payload
                    .get("attachments")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|attachment| {
                        Some(MailAttachmentRef {
                            provider: GMAIL_PROVIDER.to_string(),
                            attachment_id: attachment.get("attachmentId")?.as_str()?.to_string(),
                            filename: attachment
                                .get("filename")
                                .and_then(Value::as_str)
                                .unwrap_or("attachment")
                                .to_string(),
                            mime_type: attachment
                                .get("mimeType")
                                .and_then(Value::as_str)
                                .unwrap_or("application/octet-stream")
                                .to_string(),
                            size: attachment
                                .get("size")
                                .and_then(Value::as_u64)
                                .and_then(|size| usize::try_from(size).ok())
                                .unwrap_or_default(),
                        })
                    })
                    .collect();
                Ok(MailMessageEvidence {
                    message_ref,
                    from: payload
                        .get("from")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    to: split_recipients(
                        payload
                            .get("to")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    subject: payload
                        .get("subject")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    date: payload
                        .get("date")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    body: compact_mail_body(
                        payload
                            .get("body")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    body_source: payload
                        .get("body_source")
                        .and_then(Value::as_str)
                        .unwrap_or("full")
                        .to_string(),
                    attachments,
                })
            }
        }
    }

    pub async fn download_attachment(
        &self,
        message_ref: &MessageRef,
        attachment: &MailAttachmentRef,
    ) -> anyhow::Result<Vec<u8>> {
        let (provider, provider_account_id) = Self::split_account_id(&message_ref.account_id)?;
        match provider {
            MailProvider::Imap => {
                anyhow::ensure!(
                    attachment.provider == IMAP_PROVIDER,
                    "Attachment provider mismatch"
                );
                self.email
                    .mail_read_attachment(
                        provider_account_id,
                        parse_imap_uid(&message_ref.external_id)?,
                        message_ref.mailbox.as_deref(),
                        &attachment.attachment_id,
                    )
                    .await
            }
            MailProvider::Gmail => {
                anyhow::ensure!(
                    attachment.provider == GMAIL_PROVIDER,
                    "Attachment provider mismatch"
                );
                let google = self
                    .google
                    .as_ref()
                    .ok_or_else(|| anyhow!("Google integration is not configured"))?;
                google
                    .mail_download_attachment(
                        provider_account_id,
                        &message_ref.external_id,
                        &attachment.attachment_id,
                    )
                    .await
            }
        }
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
                let labels = google.mail_label_values(provider_account_id).await?;
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
            ToolDefinition::for_args::<EmptyToolArgs>(
                "mail_list_accounts",
                "List all configured mail accounts across IMAP/SMTP and Gmail providers.",
            ),
            ToolDefinition::for_args::<MailSearchArgs>(
                "mail_search",
                "Search email consistently across Gmail and IMAP. Prefer one structured search with several terms over overlapping provider-specific queries; paginate with next_page_token when completeness matters.",
            ),
            ToolDefinition::for_args::<MailReadArgs>(
                "mail_read",
                "Read a specific email using the message_ref returned by mail_search.",
            ),
            ToolDefinition::for_args::<MailSendArgs>(
                "mail_send",
                "Send an email through the selected unified mail account.",
            ),
            ToolDefinition::for_args::<MailboxArgs>(
                "mail_list_mailboxes",
                "List folders, mailboxes, or labels for the selected unified mail account.",
            ),
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
