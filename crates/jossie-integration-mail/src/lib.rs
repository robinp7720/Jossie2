use anyhow::{Context, anyhow};
use jossie_core::integration::{Integration, ToolDefinition};
use jossie_integration_email::EmailIntegration;
use jossie_integration_google::GoogleIntegration;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

const IMAP_PROVIDER: &str = "imap";
const GMAIL_PROVIDER: &str = "gmail";

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
    query: String,
    #[serde(default)]
    mailbox: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
    #[serde(default)]
    page_token: Option<String>,
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
        let raw = self.email.execute("email_list_accounts", "{}").await?;
        let accounts = Self::parse_json_value(&raw, "email_list_accounts")?
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

        let raw = google.execute("google_list_accounts", "{}").await?;
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
        let raw = self
            .email
            .execute(
                "email_search",
                &json!({
                    "account_id": provider_account_id,
                    "query": args.query,
                    "folder": args.mailbox.clone().unwrap_or_default(),
                })
                .to_string(),
            )
            .await?;
        let messages = Self::parse_json_value(&raw, "email_search")?
            .as_array()
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
            "next_page_token": Value::Null,
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
        let query = merge_gmail_query(args.mailbox.as_deref(), &args.query);
        let raw = google
            .execute(
                "gmail_search",
                &json!({
                    "account_id": provider_account_id,
                    "query": query,
                    "max_results": args.max_results,
                    "page_token": args.page_token,
                })
                .to_string(),
            )
            .await?;
        let payload = Self::parse_json_value(&raw, "gmail_search")?;
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
        let raw = self
            .email
            .execute(
                "email_read",
                &json!({
                    "account_id": provider_account_id,
                    "uid": parse_imap_uid(&message_ref.external_id)?,
                    "folder": message_ref.mailbox.clone().unwrap_or_default(),
                })
                .to_string(),
            )
            .await?;
        let payload = Self::parse_json_value(&raw, "email_read")?;
        Ok(serde_json::to_string_pretty(&json!({
            "message_ref": message_ref,
            "from": payload.get("from").and_then(|value| value.as_str()).unwrap_or_default(),
            "to": payload.get("to").and_then(|value| value.as_array()).cloned().unwrap_or_default(),
            "subject": payload.get("subject").and_then(|value| value.as_str()).unwrap_or_default(),
            "date": payload.get("date").and_then(|value| value.as_str()).unwrap_or_default(),
            "body": payload.get("body").and_then(|value| value.as_str()).unwrap_or_default(),
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
            .execute(
                "gmail_read",
                &json!({
                    "account_id": provider_account_id,
                    "message_id": message_ref.external_id,
                })
                .to_string(),
            )
            .await?;
        let payload = Self::parse_json_value(&raw, "gmail_read")?;
        Ok(serde_json::to_string_pretty(&json!({
            "message_ref": message_ref,
            "from": payload.get("from").and_then(|value| value.as_str()).unwrap_or_default(),
            "to": split_recipients(payload.get("to").and_then(|value| value.as_str()).unwrap_or_default()),
            "subject": payload.get("subject").and_then(|value| value.as_str()).unwrap_or_default(),
            "date": payload.get("date").and_then(|value| value.as_str()).unwrap_or_default(),
            "body": payload.get("body").and_then(|value| value.as_str()).unwrap_or_default(),
            "attachments": payload.get("attachments").cloned().unwrap_or_else(|| json!([])),
            "mailbox": message_ref.mailbox,
        }))?)
    }

    async fn mail_send(&self, args: MailSendArgs) -> anyhow::Result<String> {
        let (provider, provider_account_id) = Self::split_account_id(&args.account_id)?;
        let result = match provider {
            MailProvider::Imap => {
                self.email
                    .execute(
                        "email_send",
                        &json!({
                            "account_id": provider_account_id,
                            "to": args.to,
                            "subject": args.subject,
                            "body": args.body,
                        })
                        .to_string(),
                    )
                    .await?
            }
            MailProvider::Gmail => {
                let google = self
                    .google
                    .as_ref()
                    .ok_or_else(|| anyhow!("Google integration is not configured"))?;
                google
                    .execute(
                        "gmail_send",
                        &json!({
                            "account_id": provider_account_id,
                            "to": args.to,
                            "subject": args.subject,
                            "body": args.body,
                        })
                        .to_string(),
                    )
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
                let raw = self
                    .email
                    .execute(
                        "email_list_folders",
                        &json!({ "account_id": provider_account_id }).to_string(),
                    )
                    .await?;
                let folders = Self::parse_json_value(&raw, "email_list_folders")?
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                folders
                    .into_iter()
                    .filter_map(|folder| {
                        folder.as_str().map(|name| {
                            json!({
                                "name": name,
                                "display_name": name,
                                "kind": "folder",
                            })
                        })
                    })
                    .collect::<Vec<_>>()
            }
            MailProvider::Gmail => {
                let google = self
                    .google
                    .as_ref()
                    .ok_or_else(|| anyhow!("Google integration is not configured"))?;
                let raw = google
                    .execute(
                        "gmail_list_labels",
                        &json!({ "account_id": provider_account_id }).to_string(),
                    )
                    .await?;
                let labels = Self::parse_json_value(&raw, "gmail_list_labels")?
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
                    "Search email messages in a unified way across Gmail and IMAP-backed accounts."
                        .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "account_id": {"type": "string", "description": "Unified mail account ID from mail_list_accounts"},
                        "query": {"type": "string", "description": "Search query for the selected mailbox/account"},
                        "mailbox": {"type": ["string", "null"], "description": "Optional mailbox, folder, or Gmail label filter"},
                        "max_results": {"type": ["integer", "null"], "description": "Optional provider-specific page size hint"},
                        "page_token": {"type": ["string", "null"], "description": "Optional pagination token for providers that support it"}
                    },
                    "required": ["account_id", "query", "mailbox", "max_results", "page_token"],
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
        MailIntegration, MailProvider, MessageRef, header_value, merge_gmail_query,
        split_recipients,
    };
    use serde_json::json;

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
}
