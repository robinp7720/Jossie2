use base64::Engine;
use hmac::{Hmac, Mac};
use jossie_core::config::TodoistConfig;
use jossie_core::integration::{
    EmptyToolArgs, Integration, OAuthAccount, OnboardingStatus, ToolDefinition,
};
use jossie_db::Database;
use jossie_integration_google::GoogleIntegration;
use serde::Deserialize;
use sha2::Sha256;
use std::sync::Arc;

use crate::{account_data, response_json, spec, token_field};

pub struct TasksIntegration {
    db: Arc<Database>,
    google: Option<Arc<GoogleIntegration>>,
    config: TodoistConfig,
    client: reqwest::Client,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProjectArgs {
    account_id: String,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    account_id: String,
    project_id: String,
    #[schemars(required)]
    show_completed: Option<bool>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    account_id: String,
    project_id: String,
    title: String,
    #[schemars(required)]
    notes: Option<String>,
    #[schemars(required)]
    due: Option<String>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    account_id: String,
    project_id: String,
    task_id: String,
    #[schemars(required)]
    title: Option<String>,
    #[schemars(required)]
    notes: Option<String>,
    #[schemars(required)]
    due: Option<String>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompleteArgs {
    account_id: String,
    project_id: String,
    task_id: String,
}

impl TasksIntegration {
    pub fn new(
        db: Arc<Database>,
        google: Option<Arc<GoogleIntegration>>,
        config: &TodoistConfig,
    ) -> Self {
        Self {
            db,
            google,
            config: config.clone(),
            client: reqwest::Client::new(),
        }
    }
    async fn account(&self, id: &str) -> anyhow::Result<jossie_db::IntegrationAccount> {
        self.db
            .get_integration_account(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Task account not found"))
    }
    async fn todoist(&self, id: &str) -> anyhow::Result<(jossie_db::IntegrationAccount, String)> {
        let account = self.account(id).await?;
        anyhow::ensure!(
            account.integration == "todoist",
            "Account is not a Todoist account"
        );
        let token = account_data(&account)?
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        anyhow::ensure!(!token.is_empty(), "Todoist account has no access token");
        Ok((account, token))
    }
    async fn projects(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let account = self.account(id).await?;
        if account.integration == "google" {
            return self
                .google
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Google is unavailable"))?
                .task_lists(id)
                .await;
        }
        let (_, token) = self.todoist(id).await?;
        response_json(
            self.client
                .get("https://api.todoist.com/api/v1/projects")
                .bearer_auth(token)
                .send()
                .await?,
            "Todoist projects",
        )
        .await
    }
    async fn list(&self, args: &ListArgs) -> anyhow::Result<serde_json::Value> {
        let account = self.account(&args.account_id).await?;
        if account.integration == "google" {
            return self
                .google
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Google is unavailable"))?
                .tasks_list(
                    &args.account_id,
                    &args.project_id,
                    args.show_completed.unwrap_or(false),
                )
                .await;
        }
        let (_, token) = self.todoist(&args.account_id).await?;
        response_json(
            self.client
                .get("https://api.todoist.com/api/v1/tasks")
                .bearer_auth(token)
                .query(&[("project_id", &args.project_id)])
                .send()
                .await?,
            "Todoist tasks",
        )
        .await
    }
    async fn create(&self, args: &CreateArgs) -> anyhow::Result<serde_json::Value> {
        let account = self.account(&args.account_id).await?;
        if account.integration == "google" {
            return self
                .google
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Google is unavailable"))?
                .task_create(
                    &args.account_id,
                    &args.project_id,
                    &args.title,
                    args.notes.as_deref(),
                    args.due.as_deref(),
                )
                .await;
        }
        let (_, token) = self.todoist(&args.account_id).await?;
        let mut body = serde_json::json!({"content": args.title, "project_id": args.project_id});
        if let Some(notes) = &args.notes {
            body["description"] = notes.clone().into();
        }
        if let Some(due) = &args.due {
            body["due_string"] = due.clone().into();
        }
        response_json(
            self.client
                .post("https://api.todoist.com/api/v1/tasks")
                .bearer_auth(token)
                .json(&body)
                .send()
                .await?,
            "Todoist task creation",
        )
        .await
    }
    async fn update(&self, args: &UpdateArgs) -> anyhow::Result<serde_json::Value> {
        let account = self.account(&args.account_id).await?;
        let mut body = serde_json::Map::new();
        if let Some(title) = &args.title {
            body.insert(
                if account.integration == "google" {
                    "title"
                } else {
                    "content"
                }
                .into(),
                title.clone().into(),
            );
        }
        if let Some(notes) = &args.notes {
            body.insert(
                if account.integration == "google" {
                    "notes"
                } else {
                    "description"
                }
                .into(),
                notes.clone().into(),
            );
        }
        if let Some(due) = &args.due {
            body.insert(
                if account.integration == "google" {
                    "due"
                } else {
                    "due_string"
                }
                .into(),
                due.clone().into(),
            );
        }
        anyhow::ensure!(!body.is_empty(), "At least one task field must be changed");
        if account.integration == "google" {
            return self
                .google
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Google is unavailable"))?
                .task_patch(
                    &args.account_id,
                    &args.project_id,
                    &args.task_id,
                    body.into(),
                )
                .await;
        }
        let (_, token) = self.todoist(&args.account_id).await?;
        let url = format!(
            "https://api.todoist.com/api/v1/tasks/{}",
            urlencoding::encode(&args.task_id)
        );
        response_json(
            self.client
                .post(url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await?,
            "Todoist task update",
        )
        .await
    }
    async fn complete(&self, args: &CompleteArgs) -> anyhow::Result<serde_json::Value> {
        let account = self.account(&args.account_id).await?;
        if account.integration == "google" {
            return self
                .google
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Google is unavailable"))?
                .task_patch(
                    &args.account_id,
                    &args.project_id,
                    &args.task_id,
                    serde_json::json!({"status":"completed"}),
                )
                .await;
        }
        let (_, token) = self.todoist(&args.account_id).await?;
        let url = format!(
            "https://api.todoist.com/api/v1/tasks/{}/close",
            urlencoding::encode(&args.task_id)
        );
        response_json(
            self.client.post(url).bearer_auth(token).send().await?,
            "Todoist task completion",
        )
        .await
    }
    async fn poll_todoist(&self, account: &jossie_db::IntegrationAccount) -> anyhow::Result<()> {
        let seed_key = format!("poll_seeded:{}", account.id);
        let seeded = self
            .db
            .get_integration_setting("todoist", &seed_key)
            .await?
            .is_some();
        let (_, token) = self.todoist(&account.id).await?;
        let value = response_json(
            self.client
                .get("https://api.todoist.com/api/v1/tasks")
                .bearer_auth(token)
                .send()
                .await?,
            "Todoist polling",
        )
        .await?;
        if !seeded {
            self.db
                .set_integration_setting("todoist", &seed_key, &chrono::Utc::now().to_rfc3339())
                .await?;
            return Ok(());
        }
        let now = chrono::Utc::now();
        let horizon = now + chrono::Duration::hours(24);
        let tasks = value
            .as_array()
            .cloned()
            .or_else(|| value.get("results").and_then(|v| v.as_array()).cloned())
            .unwrap_or_default();
        for task in tasks {
            let Some(due) = task.get("due") else { continue };
            let raw_due = due
                .get("datetime")
                .or_else(|| due.get("date"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let parsed = chrono::DateTime::parse_from_rfc3339(raw_due)
                .map(|v| v.with_timezone(&chrono::Utc))
                .or_else(|_| {
                    chrono::NaiveDate::parse_from_str(raw_due, "%Y-%m-%d")
                        .map(|d| d.and_hms_opt(9, 0, 0).expect("valid hour").and_utc())
                });
            let Ok(due_at) = parsed else { continue };
            if due_at < now - chrono::Duration::minutes(5) || due_at > horizon {
                continue;
            }
            let task_id = task.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            if task_id.is_empty() {
                continue;
            }
            let dedupe = format!("{task_id}:{raw_due}");
            self.db.insert_integration_event("todoist", &account.id, jossie_core::events::TASK_DUE, &dedupe, &serde_json::json!({
                "task_id": task_id, "title": task.get("content"), "description": task.get("description"), "due": due,
                "project_id": task.get("project_id"), "url": task.get("url")
            })).await?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Integration for TasksIntegration {
    fn name(&self) -> &str {
        "tasks"
    }
    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::for_args::<EmptyToolArgs>(
                "task_list_accounts",
                "List Google Tasks and Todoist accounts.",
            ),
            ToolDefinition::for_args::<ProjectArgs>(
                "task_list_projects",
                "List task lists or projects for an account.",
            ),
            ToolDefinition::for_args::<ListArgs>(
                "task_list",
                "List tasks in a task list or project.",
            ),
            ToolDefinition::for_args::<CreateArgs>(
                "task_create",
                "Create a task. This changes an external task system and requires approval.",
            ),
            ToolDefinition::for_args::<UpdateArgs>(
                "task_update",
                "Update an exact task. This changes an external task system and requires approval.",
            ),
            ToolDefinition::for_args::<CompleteArgs>(
                "task_complete",
                "Complete an exact task. This changes an external task system and requires approval.",
            ),
        ]
    }
    fn connection_spec(&self) -> Option<jossie_core::integration::ConnectionSpec> {
        Some(spec(
            "todoist",
            "Todoist",
            "Tasks, projects, due dates, and completion",
            vec![token_field()],
            !self.config.client_id.is_empty(),
        ))
    }
    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        if !self
            .db
            .list_integration_accounts("google")
            .await?
            .is_empty()
            || !self
                .db
                .list_integration_accounts("todoist")
                .await?
                .is_empty()
        {
            Ok(OnboardingStatus::Configured)
        } else {
            Ok(OnboardingStatus::RequiresAction { fields: Vec::new() })
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
        let url = format!(
            "https://todoist.com/oauth/authorize?client_id={}&scope=data:read_write&state={}&redirect_uri={}",
            urlencoding::encode(&self.config.client_id),
            urlencoding::encode(state),
            urlencoding::encode(redirect_uri)
        );
        Ok(Some(url))
    }
    async fn oauth_exchange(&self, code: &str, redirect_uri: &str) -> anyhow::Result<OAuthAccount> {
        let value = response_json(
            self.client
                .post("https://todoist.com/oauth/access_token")
                .form(&[
                    ("client_id", self.config.client_id.as_str()),
                    ("client_secret", self.config.client_secret.as_str()),
                    ("code", code),
                    ("redirect_uri", redirect_uri),
                ])
                .send()
                .await?,
            "Todoist OAuth exchange",
        )
        .await?;
        let token = value
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Todoist did not return an access token"))?
            .to_string();
        let user = response_json(
            self.client
                .get("https://api.todoist.com/api/v1/user")
                .bearer_auth(&token)
                .send()
                .await?,
            "Todoist profile",
        )
        .await
        .unwrap_or_default();
        Ok(OAuthAccount {
            name: "Todoist account".into(),
            data: serde_json::json!({
                "access_token": token,
                "user_id": user.get("id").map(|value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())).unwrap_or_default(),
                "source":"oauth"
            }),
        })
    }
    async fn handle_webhook(
        &self,
        headers: &std::collections::HashMap<String, String>,
        body: &[u8],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.config.client_secret.is_empty(),
            "Todoist webhook secret is not configured"
        );
        let signature = headers
            .get("x-todoist-hmac-sha256")
            .ok_or_else(|| anyhow::anyhow!("Missing Todoist signature"))?;
        let expected = base64::engine::general_purpose::STANDARD.decode(signature)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(self.config.client_secret.as_bytes())?;
        mac.update(body);
        mac.verify_slice(&expected)
            .map_err(|_| anyhow::anyhow!("Invalid Todoist webhook signature"))?;

        let payload: serde_json::Value = serde_json::from_slice(body)?;
        let delivery = headers
            .get("x-todoist-delivery-id")
            .cloned()
            .or_else(|| {
                payload
                    .get("event_data")
                    .and_then(|value| value.get("id"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow::anyhow!("Todoist webhook has no delivery id"))?;
        let accounts = self.db.list_integration_accounts("todoist").await?;
        let webhook_user = payload.get("user_id").map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string())
        });
        let account = if let Some(user_id) = webhook_user.as_deref() {
            accounts.iter().find(|account| {
                account_data(account)
                    .ok()
                    .and_then(|data| {
                        data.get("user_id")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    })
                    .as_deref()
                    == Some(user_id)
            })
        } else if accounts.len() == 1 {
            accounts.first()
        } else {
            None
        }
        .ok_or_else(|| {
            anyhow::anyhow!("Todoist webhook could not be matched to a connected account")
        })?;
        self.db
            .insert_integration_event(
                "todoist",
                &account.id,
                jossie_core::events::TASK_CHANGED,
                &delivery,
                &payload,
            )
            .await?;
        Ok(())
    }
    async fn execute(&self, name: &str, arguments: &str) -> anyhow::Result<String> {
        let value = match name {
            "task_list_accounts" => {
                let mut accounts = self.db.list_integration_accounts("google").await?;
                accounts.extend(self.db.list_integration_accounts("todoist").await?);
                serde_json::json!(accounts.into_iter().map(|a| serde_json::json!({"id":a.id,"provider":a.integration,"name":a.name})).collect::<Vec<_>>())
            }
            "task_list_projects" => {
                self.projects(&serde_json::from_str::<ProjectArgs>(arguments)?.account_id)
                    .await?
            }
            "task_list" => self.list(&serde_json::from_str(arguments)?).await?,
            "task_create" => self.create(&serde_json::from_str(arguments)?).await?,
            "task_update" => self.update(&serde_json::from_str(arguments)?).await?,
            "task_complete" => self.complete(&serde_json::from_str(arguments)?).await?,
            _ => anyhow::bail!("Unknown tasks tool: {name}"),
        };
        Ok(serde_json::to_string_pretty(&value)?)
    }
    async fn poll(&self) -> anyhow::Result<()> {
        for account in self.db.list_integration_accounts("todoist").await? {
            if let Err(error) = self.poll_todoist(&account).await {
                tracing::warn!("Todoist poll failed for account {}: {error}", account.id);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn todoist_webhook_verifies_signature_and_deduplicates_delivery() {
        let db = Arc::new(Database::new("sqlite::memory:").await.unwrap());
        db.migrate().await.unwrap();
        db.add_integration_account(
            "todoist",
            "Tasks",
            &serde_json::json!({"access_token":"token","user_id":"42"}),
        )
        .await
        .unwrap();
        let integration = TasksIntegration::new(
            db.clone(),
            None,
            &TodoistConfig {
                client_id: "client".into(),
                client_secret: "webhook-secret".into(),
            },
        );
        let body = br#"{"event_name":"item:updated","user_id":"42","event_data":{"id":"task-1"}}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"webhook-secret").unwrap();
        mac.update(body);
        let signature =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        let headers = std::collections::HashMap::from([
            ("x-todoist-hmac-sha256".into(), signature),
            ("x-todoist-delivery-id".into(), "delivery-1".into()),
        ]);

        integration.handle_webhook(&headers, body).await.unwrap();
        integration.handle_webhook(&headers, body).await.unwrap();
        let events = db.list_pending_integration_events(10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, jossie_core::events::TASK_CHANGED);

        let invalid = std::collections::HashMap::from([
            ("x-todoist-hmac-sha256".into(), "aW52YWxpZA==".into()),
            ("x-todoist-delivery-id".into(), "delivery-2".into()),
        ]);
        assert!(integration.handle_webhook(&invalid, body).await.is_err());
    }
}
