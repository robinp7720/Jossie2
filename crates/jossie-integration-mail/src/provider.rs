use super::{
    GMAIL_PROVIDER, IMAP_PROVIDER, MailSearchArgs, build_gmail_search_query,
    normalize_optional_mailbox, parse_imap_uid, split_recipients,
};
use jossie_integration_email::{EmailIntegration, EmailSearchRequest};
use jossie_integration_google::GoogleIntegration;
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(super) struct ProviderAccount {
    pub id: String,
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone)]
pub(super) struct ProviderSearchPage {
    pub messages: Vec<ProviderSearchMessage>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ProviderSearchMessage {
    pub external_id: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub date: String,
    pub snippet: String,
    pub mailbox: Option<String>,
    pub native: Option<Value>,
}

#[derive(Debug, Clone)]
pub(super) struct ProviderAttachment {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub(super) struct ProviderMessage {
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub date: String,
    pub body: String,
    pub body_source: String,
    pub attachments: Vec<ProviderAttachment>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct ProviderMailbox {
    pub name: String,
    pub display_name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[async_trait::async_trait]
pub(super) trait MailProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn list_accounts(&self) -> anyhow::Result<Vec<ProviderAccount>>;
    async fn search(
        &self,
        account_id: &str,
        request: &MailSearchArgs,
    ) -> anyhow::Result<ProviderSearchPage>;
    async fn read(
        &self,
        account_id: &str,
        external_id: &str,
        mailbox: Option<&str>,
    ) -> anyhow::Result<ProviderMessage>;
    async fn download_attachment(
        &self,
        account_id: &str,
        external_id: &str,
        mailbox: Option<&str>,
        attachment_id: &str,
    ) -> anyhow::Result<Vec<u8>>;
    async fn send(
        &self,
        account_id: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> anyhow::Result<String>;
    async fn list_mailboxes(&self, account_id: &str) -> anyhow::Result<Vec<ProviderMailbox>>;
}

pub(super) struct ImapMailProvider {
    integration: Arc<EmailIntegration>,
}

impl ImapMailProvider {
    pub fn new(integration: Arc<EmailIntegration>) -> Self {
        Self { integration }
    }
}

#[async_trait::async_trait]
impl MailProvider for ImapMailProvider {
    fn name(&self) -> &'static str {
        IMAP_PROVIDER
    }

    async fn list_accounts(&self) -> anyhow::Result<Vec<ProviderAccount>> {
        Ok(self
            .integration
            .mail_accounts()
            .await?
            .into_iter()
            .map(|account| ProviderAccount {
                id: string_field(&account, "id"),
                name: string_field(&account, "name"),
                email: string_field(&account, "email"),
            })
            .collect())
    }

    async fn search(
        &self,
        account_id: &str,
        request: &MailSearchArgs,
    ) -> anyhow::Result<ProviderSearchPage> {
        let payload = self
            .integration
            .mail_search(
                account_id,
                EmailSearchRequest {
                    query: request.query.clone(),
                    terms: request.terms.clone(),
                    match_mode: request.r#match.clone(),
                    from: request.from.clone(),
                    subject: request.subject.clone(),
                    after: request.after.clone(),
                    before: request.before.clone(),
                    max_results: request.max_results,
                    page_token: request.page_token.clone(),
                    folder: request.mailbox.clone(),
                },
            )
            .await?;
        let mailbox = normalize_optional_mailbox(request.mailbox.as_deref())
            .or_else(|| Some("INBOX".to_string()));
        let messages = payload
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|message| {
                let external_id = message
                    .get("uid")
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    .to_string();
                ProviderSearchMessage {
                    native: Some(serde_json::json!({
                        "uid": external_id.parse::<u32>().ok(),
                    })),
                    external_id,
                    from: string_field(message, "from"),
                    to: Vec::new(),
                    subject: string_field(message, "subject"),
                    date: string_field(message, "date"),
                    snippet: String::new(),
                    mailbox: mailbox.clone(),
                }
            })
            .collect();
        Ok(ProviderSearchPage {
            messages,
            next_page_token: payload
                .get("next_page_token")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn read(
        &self,
        account_id: &str,
        external_id: &str,
        mailbox: Option<&str>,
    ) -> anyhow::Result<ProviderMessage> {
        let content = self
            .integration
            .mail_read_content(account_id, parse_imap_uid(external_id)?, mailbox)
            .await?;
        Ok(ProviderMessage {
            from: content.from,
            to: content.to,
            subject: content.subject,
            date: content.date,
            body: content.body,
            body_source: content.body_source,
            attachments: content
                .attachments
                .into_iter()
                .map(|attachment| ProviderAttachment {
                    id: attachment.part_id,
                    filename: attachment.filename,
                    mime_type: attachment.mime_type,
                    size: attachment.size,
                })
                .collect(),
        })
    }

    async fn download_attachment(
        &self,
        account_id: &str,
        external_id: &str,
        mailbox: Option<&str>,
        attachment_id: &str,
    ) -> anyhow::Result<Vec<u8>> {
        self.integration
            .mail_read_attachment(
                account_id,
                parse_imap_uid(external_id)?,
                mailbox,
                attachment_id,
            )
            .await
    }

    async fn send(
        &self,
        account_id: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        self.integration
            .mail_send(account_id, to, subject, body)
            .await
    }

    async fn list_mailboxes(&self, account_id: &str) -> anyhow::Result<Vec<ProviderMailbox>> {
        Ok(self
            .integration
            .mail_folders(account_id)
            .await?
            .into_iter()
            .map(|name| ProviderMailbox {
                display_name: name.clone(),
                name,
                kind: "folder".to_string(),
                id: None,
            })
            .collect())
    }
}

pub(super) struct GmailMailProvider {
    integration: Arc<GoogleIntegration>,
}

impl GmailMailProvider {
    pub fn new(integration: Arc<GoogleIntegration>) -> Self {
        Self { integration }
    }
}

#[async_trait::async_trait]
impl MailProvider for GmailMailProvider {
    fn name(&self) -> &'static str {
        GMAIL_PROVIDER
    }

    async fn list_accounts(&self) -> anyhow::Result<Vec<ProviderAccount>> {
        Ok(self
            .integration
            .account_values()
            .await?
            .into_iter()
            .map(|account| ProviderAccount {
                id: string_field(&account, "id"),
                name: string_field(&account, "name"),
                email: string_field(&account, "email"),
            })
            .collect())
    }

    async fn search(
        &self,
        account_id: &str,
        request: &MailSearchArgs,
    ) -> anyhow::Result<ProviderSearchPage> {
        let payload = self
            .integration
            .mail_search_value(
                account_id,
                &build_gmail_search_query(request),
                request.max_results,
                request.page_token.as_deref(),
            )
            .await?;
        let mailbox = normalize_optional_mailbox(request.mailbox.as_deref());
        let messages = payload
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|message| {
                let headers = message
                    .get("headers")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                ProviderSearchMessage {
                    external_id: string_field(message, "id"),
                    from: super::header_value(&headers, "From"),
                    to: split_recipients(&super::header_value(&headers, "To")),
                    subject: super::header_value(&headers, "Subject"),
                    date: super::header_value(&headers, "Date"),
                    snippet: string_field(message, "snippet"),
                    mailbox: mailbox.clone(),
                    native: None,
                }
            })
            .collect();
        Ok(ProviderSearchPage {
            messages,
            next_page_token: payload
                .get("next_page_token")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn read(
        &self,
        account_id: &str,
        external_id: &str,
        _mailbox: Option<&str>,
    ) -> anyhow::Result<ProviderMessage> {
        let payload = self
            .integration
            .mail_read_value(account_id, external_id)
            .await?;
        let attachments = payload
            .get("attachments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|attachment| {
                Some(ProviderAttachment {
                    id: attachment.get("attachmentId")?.as_str()?.to_string(),
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
        Ok(ProviderMessage {
            from: string_field(&payload, "from"),
            to: split_recipients(&string_field(&payload, "to")),
            subject: string_field(&payload, "subject"),
            date: string_field(&payload, "date"),
            body: string_field(&payload, "body"),
            body_source: payload
                .get("body_source")
                .and_then(Value::as_str)
                .unwrap_or("full")
                .to_string(),
            attachments,
        })
    }

    async fn download_attachment(
        &self,
        account_id: &str,
        external_id: &str,
        _mailbox: Option<&str>,
        attachment_id: &str,
    ) -> anyhow::Result<Vec<u8>> {
        self.integration
            .mail_download_attachment(account_id, external_id, attachment_id)
            .await
    }

    async fn send(
        &self,
        account_id: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        self.integration
            .mail_send(account_id, to, subject, body)
            .await
    }

    async fn list_mailboxes(&self, account_id: &str) -> anyhow::Result<Vec<ProviderMailbox>> {
        Ok(self
            .integration
            .mail_label_values(account_id)
            .await?
            .into_iter()
            .map(|label| {
                let name = string_field(&label, "name");
                ProviderMailbox {
                    display_name: name.clone(),
                    name,
                    kind: label
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("label")
                        .to_ascii_lowercase(),
                    id: label.get("id").and_then(Value::as_str).map(str::to_string),
                }
            })
            .collect())
    }
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
