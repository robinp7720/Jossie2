use crate::{account_data, response_json, spec, token_field};
use base64::Engine;
use jossie_core::config::NotionConfig;
use jossie_core::integration::{Integration, OAuthAccount, OnboardingStatus, ToolDefinition};
use jossie_db::Database;
use serde::Deserialize;
use std::sync::Arc;

pub struct NotionIntegration {
    db: Arc<Database>,
    config: NotionConfig,
    client: reqwest::Client,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    account_id: String,
    query: String,
    #[schemars(required)]
    page_size: Option<u32>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    account_id: String,
    page_id: String,
    #[schemars(required)]
    page_size: Option<u32>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    account_id: String,
    parent_page_id: String,
    title: String,
    #[schemars(required)]
    content: Option<String>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AppendArgs {
    account_id: String,
    block_id: String,
    content: String,
}

impl NotionIntegration {
    pub fn new(db: Arc<Database>, config: &NotionConfig) -> Self {
        Self {
            db,
            config: config.clone(),
            client: reqwest::Client::new(),
        }
    }
    async fn token(&self, id: &str) -> anyhow::Result<String> {
        let a = self
            .db
            .get_integration_account(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Notion account not found"))?;
        anyhow::ensure!(a.integration == "notion", "Account is not Notion");
        Ok(account_data(&a)?
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string())
    }
    async fn request(
        &self,
        id: &str,
        method: reqwest::Method,
        url: String,
        body: Option<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let mut r = self
            .client
            .request(method, url)
            .bearer_auth(self.token(id).await?)
            .header("Notion-Version", "2025-09-03");
        if let Some(body) = body {
            r = r.json(&body);
        }
        response_json(r.send().await?, "Notion request").await
    }
    fn paragraph(content: &str) -> serde_json::Value {
        serde_json::json!({"object":"block","type":"paragraph","paragraph":{"rich_text":[{"type":"text","text":{"content":content}}]}})
    }
}
#[async_trait::async_trait]
impl Integration for NotionIntegration {
    fn name(&self) -> &str {
        "notion"
    }
    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::for_args::<SearchArgs>(
                "notes_search",
                "Search titles of Notion pages explicitly shared with Jossie.",
            ),
            ToolDefinition::for_args::<ReadArgs>(
                "notes_read",
                "Read blocks from an exact Notion page or block.",
            ),
            ToolDefinition::for_args::<CreateArgs>(
                "notes_create_page",
                "Create a page beneath an exact shared Notion page. Requires approval.",
            ),
            ToolDefinition::for_args::<AppendArgs>(
                "notes_append",
                "Append a paragraph to an exact Notion block. Requires approval.",
            ),
        ]
    }
    fn connection_spec(&self) -> Option<jossie_core::integration::ConnectionSpec> {
        Some(spec(
            "notion",
            "Notion",
            "Search selected pages and write approved notes",
            vec![token_field()],
            !self.config.client_id.is_empty(),
        ))
    }
    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        if self
            .db
            .list_integration_accounts("notion")
            .await?
            .is_empty()
        {
            Ok(OnboardingStatus::RequiresAction { fields: Vec::new() })
        } else {
            Ok(OnboardingStatus::Configured)
        }
    }
    fn oauth_authorization_url(
        &self,
        redirect_uri: &str,
        state: &str,
    ) -> anyhow::Result<Option<String>> {
        if self.config.client_id.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(
            "https://api.notion.com/v1/oauth/authorize?client_id={}&response_type=code&owner=user&redirect_uri={}&state={}",
            urlencoding::encode(&self.config.client_id),
            urlencoding::encode(redirect_uri),
            urlencoding::encode(state)
        )))
    }
    async fn oauth_exchange(&self, code: &str, redirect_uri: &str) -> anyhow::Result<OAuthAccount> {
        let auth = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            self.config.client_id, self.config.client_secret
        ));
        let v = response_json(
            self.client
                .post("https://api.notion.com/v1/oauth/token")
                .header("Authorization", format!("Basic {auth}"))
                .json(&serde_json::json!({
                    "grant_type": "authorization_code",
                    "code": code,
                    "redirect_uri": redirect_uri,
                }))
                .send()
                .await?,
            "Notion OAuth exchange",
        )
        .await?;
        let token = v
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Notion did not return an access token"))?;
        let name = v
            .get("workspace_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Notion workspace");
        Ok(OAuthAccount {
            name: name.into(),
            data: serde_json::json!({"access_token":token,"workspace_id":v.get("workspace_id"),"source":"oauth"}),
        })
    }
    async fn execute(&self, name: &str, arguments: &str) -> anyhow::Result<String> {
        let v = match name {
            "notes_search" => {
                let a: SearchArgs = serde_json::from_str(arguments)?;
                self.request(
                    &a.account_id,
                    reqwest::Method::POST,
                    "https://api.notion.com/v1/search".into(),
                    Some(serde_json::json!({
                        "query": a.query,
                        "page_size": a.page_size.unwrap_or(20).min(100),
                    })),
                )
                .await?
            }
            "notes_read" => {
                let a: ReadArgs = serde_json::from_str(arguments)?;
                self.request(
                    &a.account_id,
                    reqwest::Method::GET,
                    format!(
                        "https://api.notion.com/v1/blocks/{}/children?page_size={}",
                        urlencoding::encode(&a.page_id),
                        a.page_size.unwrap_or(50).min(100)
                    ),
                    None,
                )
                .await?
            }
            "notes_create_page" => {
                let a: CreateArgs = serde_json::from_str(arguments)?;
                let mut b = serde_json::json!({"parent":{"type":"page_id","page_id":a.parent_page_id},"properties":{"title":{"type":"title","title":[{"type":"text","text":{"content":a.title}}]}}});
                if let Some(c) = a.content {
                    b["children"] = serde_json::json!([Self::paragraph(&c)]);
                }
                self.request(
                    &a.account_id,
                    reqwest::Method::POST,
                    "https://api.notion.com/v1/pages".into(),
                    Some(b),
                )
                .await?
            }
            "notes_append" => {
                let a: AppendArgs = serde_json::from_str(arguments)?;
                self.request(
                    &a.account_id,
                    reqwest::Method::PATCH,
                    format!(
                        "https://api.notion.com/v1/blocks/{}/children",
                        urlencoding::encode(&a.block_id)
                    ),
                    Some(serde_json::json!({"children":[Self::paragraph(&a.content)]})),
                )
                .await?
            }
            _ => anyhow::bail!("Unknown Notion tool: {name}"),
        };
        Ok(serde_json::to_string_pretty(&v)?)
    }
}
