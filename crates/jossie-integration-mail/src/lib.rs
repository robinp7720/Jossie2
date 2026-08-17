use anyhow::{Context, anyhow};
use jossie_core::integration::{EmptyToolArgs, Integration, ToolDefinition};
use jossie_integration_email::EmailIntegration;
use jossie_integration_google::GoogleIntegration;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

mod provider;

use provider::{GmailMailProvider, ImapMailProvider, MailProvider};

const IMAP_PROVIDER: &str = "imap";
const GMAIL_PROVIDER: &str = "gmail";
const MAX_UNIFIED_MAIL_BODY_CHARS: usize = 12_000;

pub struct MailIntegration {
    providers: Vec<Arc<dyn MailProvider>>,
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
        let mut providers: Vec<Arc<dyn MailProvider>> =
            vec![Arc::new(ImapMailProvider::new(email))];
        if let Some(google) = google {
            providers.push(Arc::new(GmailMailProvider::new(google)));
        }
        Self { providers }
    }

    fn unified_account_id(provider: &str, provider_account_id: &str) -> String {
        format!("{provider}:{provider_account_id}")
    }

    fn split_account_id(account_id: &str) -> anyhow::Result<(&str, &str)> {
        let (provider, provider_account_id) = account_id
            .split_once(':')
            .ok_or_else(|| anyhow!("Unsupported mail account ID: {account_id}"))?;
        if provider_account_id.trim().is_empty() {
            anyhow::bail!("Missing provider account ID in {account_id}");
        }
        Ok((provider, provider_account_id))
    }

    fn provider(&self, name: &str) -> anyhow::Result<&dyn MailProvider> {
        self.providers
            .iter()
            .find(|provider| provider.name() == name)
            .map(AsRef::as_ref)
            .ok_or_else(|| anyhow!("Unknown or unconfigured mail provider: {name}"))
    }

    async fn mail_list_accounts(&self) -> anyhow::Result<String> {
        let mut accounts = Vec::new();
        for provider in &self.providers {
            for account in provider.list_accounts().await? {
                accounts.push(json!({
                    "id": Self::unified_account_id(provider.name(), &account.id),
                    "provider": provider.name(),
                    "provider_account_id": account.id,
                    "name": account.name,
                    "email": account.email,
                    "capabilities": ["search", "read", "send", "list_mailboxes"],
                }));
            }
        }
        Ok(serde_json::to_string_pretty(&accounts)?)
    }

    async fn mail_search(&self, args: MailSearchArgs) -> anyhow::Result<String> {
        let account_id = args.account_id.clone();
        let (provider_name, provider_account_id) = Self::split_account_id(&account_id)?;
        let provider = self.provider(provider_name)?;
        let page = provider.search(provider_account_id, &args).await?;
        let normalized: Vec<Value> = page
            .messages
            .into_iter()
            .map(|message| {
                json!({
                    "message_ref": MessageRef {
                        provider: provider.name().to_string(),
                        account_id: Self::unified_account_id(provider.name(), provider_account_id),
                        external_id: message.external_id,
                        mailbox: message.mailbox.clone(),
                        native: message.native,
                    },
                    "from": message.from,
                    "to": message.to,
                    "subject": message.subject,
                    "date": message.date,
                    "snippet": message.snippet,
                    "mailbox": message.mailbox,
                })
            })
            .collect();

        Ok(serde_json::to_string_pretty(&json!({
            "messages": normalized,
            "next_page_token": page.next_page_token,
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
        let (provider_name, provider_account_id) = Self::split_account_id(&message_ref.account_id)?;
        let provider = self.provider(provider_name)?;
        anyhow::ensure!(
            message_ref.provider == provider.name(),
            "Message reference provider mismatch"
        );
        let content = provider
            .read(
                provider_account_id,
                &message_ref.external_id,
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
                    provider: provider.name().to_string(),
                    attachment_id: attachment.id,
                    filename: attachment.filename,
                    mime_type: attachment.mime_type,
                    size: attachment.size,
                })
                .collect(),
        })
    }

    pub async fn download_attachment(
        &self,
        message_ref: &MessageRef,
        attachment: &MailAttachmentRef,
    ) -> anyhow::Result<Vec<u8>> {
        let (provider_name, provider_account_id) = Self::split_account_id(&message_ref.account_id)?;
        let provider = self.provider(provider_name)?;
        anyhow::ensure!(
            message_ref.provider == provider.name() && attachment.provider == provider.name(),
            "Attachment provider mismatch"
        );
        provider
            .download_attachment(
                provider_account_id,
                &message_ref.external_id,
                message_ref.mailbox.as_deref(),
                &attachment.attachment_id,
            )
            .await
    }

    async fn mail_send(&self, args: MailSendArgs) -> anyhow::Result<String> {
        let (provider_name, provider_account_id) = Self::split_account_id(&args.account_id)?;
        let provider = self.provider(provider_name)?;
        let result = provider
            .send(provider_account_id, &args.to, &args.subject, &args.body)
            .await?;

        Ok(serde_json::to_string_pretty(&json!({
            "status": "sent",
            "provider": provider.name(),
            "account_id": args.account_id,
            "result": result,
        }))?)
    }

    async fn mail_list_mailboxes(&self, args: MailboxArgs) -> anyhow::Result<String> {
        let (provider_name, provider_account_id) = Self::split_account_id(&args.account_id)?;
        let mailboxes = self
            .provider(provider_name)?
            .list_mailboxes(provider_account_id)
            .await?;

        Ok(serde_json::to_string_pretty(&mailboxes)?)
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
        GMAIL_PROVIDER, IMAP_PROVIDER, MailIntegration, MailSearchArgs, MessageRef,
        build_gmail_search_query, compact_mail_body, header_value, merge_gmail_query,
        split_recipients,
    };
    use serde_json::json;
    use std::sync::Arc;

    use jossie_core::{config::EmailConfig, integration::Integration};
    use jossie_integration_email::EmailIntegration;

    #[test]
    fn builds_prefixed_unified_account_ids() {
        assert_eq!(
            MailIntegration::unified_account_id(IMAP_PROVIDER, "default"),
            "imap:default"
        );
        assert_eq!(
            MailIntegration::unified_account_id(GMAIL_PROVIDER, "acc_1"),
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
