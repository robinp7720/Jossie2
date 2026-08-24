use crate::{account_data, response_json, spec, token_field};
use jossie_core::integration::{ConnectionField, Integration, OnboardingStatus, ToolDefinition};
use jossie_db::Database;
use serde::Deserialize;
use std::sync::Arc;

pub struct HomeIntegration {
    db: Arc<Database>,
    client: reqwest::Client,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AccountArgs {
    account_id: Option<String>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct StateArgs {
    account_id: Option<String>,
    entity_id: String,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EntityListArgs {
    account_id: Option<String>,
    #[schemars(required)]
    domain: Option<String>,
    #[schemars(required)]
    include_sensitive: Option<bool>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct HistoryArgs {
    account_id: Option<String>,
    entity_id: String,
    start_time: String,
    #[schemars(required)]
    end_time: Option<String>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ServiceArgs {
    account_id: Option<String>,
    domain: String,
    service: String,
    #[schemars(required)]
    service_data: Option<serde_json::Value>,
}

impl HomeIntegration {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            client: reqwest::Client::new(),
        }
    }
    async fn resolve_account(
        &self,
        account_id: Option<&str>,
    ) -> anyhow::Result<jossie_db::IntegrationAccount> {
        let mut accounts = self.db.list_integration_accounts("home_assistant").await?;
        if let Some(id) = account_id.map(str::trim).filter(|id| !id.is_empty()) {
            return accounts
                .into_iter()
                .find(|account| account.id == id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Home Assistant account '{id}' not found. Omit account_id to use the only connected account, or use one of the available ids."
                    )
                });
        }
        anyhow::ensure!(
            !accounts.is_empty(),
            "No Home Assistant account is connected. Add one under Connections first."
        );
        if accounts.len() > 1 {
            anyhow::bail!(
                "Multiple Home Assistant accounts are connected; pass the exact account_id. Available: {}",
                accounts
                    .iter()
                    .map(|account| account.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok(accounts.remove(0))
    }
    async fn auth(
        &self,
        account_id: Option<&str>,
    ) -> anyhow::Result<(jossie_db::IntegrationAccount, String, String)> {
        let account = self.resolve_account(account_id).await?;
        let data = account_data(&account)?;
        let base = data
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string();
        let token = data
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let url = url::Url::parse(&base)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none(),
            "Home Assistant base URL must be HTTP(S) without embedded credentials"
        );
        anyhow::ensure!(!token.is_empty(), "Home Assistant token is missing");
        Ok((account, base, token))
    }
    async fn get(&self, account_id: Option<&str>, path: &str) -> anyhow::Result<serde_json::Value> {
        let (_, base, token) = self.auth(account_id).await?;
        response_json(
            self.client
                .get(format!("{base}{path}"))
                .bearer_auth(token)
                .send()
                .await?,
            "Home Assistant request",
        )
        .await
    }

    async fn monitored_entities(
        &self,
        account: &jossie_db::IntegrationAccount,
    ) -> anyhow::Result<Vec<String>> {
        let data = account_data(account)?;
        Ok(match data.get("monitored_entities") {
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect(),
            Some(serde_json::Value::String(value)) => value
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        })
    }

    async fn poll_account(&self, account: &jossie_db::IntegrationAccount) -> anyhow::Result<()> {
        for entity_id in self.monitored_entities(account).await? {
            let state = self
                .get(
                    Some(&account.id),
                    &format!("/api/states/{}", urlencoding::encode(&entity_id)),
                )
                .await?;
            let fingerprint = serde_json::json!({"state": state.get("state"), "last_changed": state.get("last_changed")}).to_string();
            let key = format!("state:{}:{}", account.id, entity_id);
            let previous = self
                .db
                .get_integration_setting("home_assistant", &key)
                .await?;
            self.db
                .set_integration_setting("home_assistant", &key, &fingerprint)
                .await?;
            if previous.is_some_and(|previous| previous != fingerprint) {
                let dedupe = format!(
                    "{}:{}",
                    entity_id,
                    state
                        .get("last_changed")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&fingerprint)
                );
                self.db
                    .insert_integration_event(
                        "home_assistant",
                        &account.id,
                        jossie_core::events::HOME_STATE_CHANGED,
                        &dedupe,
                        &serde_json::json!({"entity_id": entity_id, "state": state}),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Integration for HomeIntegration {
    fn name(&self) -> &str {
        "home_assistant"
    }
    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::for_args::<EntityListArgs>(
                "home_list_entities",
                "List Home Assistant entities. Presence, cameras, locks, and alarms are hidden unless include_sensitive is true. Omit account_id when only one Home Assistant account is connected.",
            ),
            ToolDefinition::for_args::<StateArgs>(
                "home_get_state",
                "Read the current state of one exact Home Assistant entity. Omit account_id when only one Home Assistant account is connected.",
            ),
            ToolDefinition::for_args::<HistoryArgs>(
                "home_get_history",
                "Read recent history for one exact Home Assistant entity. Omit account_id when only one Home Assistant account is connected.",
            ),
            ToolDefinition::for_args::<AccountArgs>(
                "home_list_services",
                "List available Home Assistant service domains and actions. Omit account_id when only one Home Assistant account is connected.",
            ),
            ToolDefinition::for_args::<ServiceArgs>(
                "home_call_service",
                "Invoke one exact Home Assistant service without a separate approval step. Omit account_id when only one Home Assistant account is connected.",
            ),
        ]
    }
    fn connection_spec(&self) -> Option<jossie_core::integration::ConnectionSpec> {
        Some(spec(
            "home_assistant",
            "Home Assistant",
            "Local home state, sensors, and service calls",
            vec![
                ConnectionField {
                    name: "base_url".into(),
                    label: "Home Assistant URL".into(),
                    input_type: "url".into(),
                    required: true,
                    secret: false,
                    description: Some("For example http://homeassistant.local:8123".into()),
                    default_value: None,
                },
                token_field(),
                ConnectionField {
                    name: "monitored_entities".into(),
                    label: "Monitored entities".into(),
                    input_type: "text".into(),
                    required: false,
                    secret: false,
                    description: Some(
                        "Optional comma-separated entity IDs for proactive state-change triage"
                            .into(),
                    ),
                    default_value: None,
                },
            ],
            false,
        ))
    }
    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> {
        if self
            .db
            .list_integration_accounts("home_assistant")
            .await?
            .is_empty()
        {
            Ok(OnboardingStatus::RequiresAction { fields: Vec::new() })
        } else {
            Ok(OnboardingStatus::Configured)
        }
    }
    async fn execute(&self, name: &str, arguments: &str) -> anyhow::Result<String> {
        let value = match name {
            "home_list_entities" => {
                let args: EntityListArgs = serde_json::from_str(arguments)?;
                let mut states = self
                    .get(args.account_id.as_deref(), "/api/states")
                    .await?
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if let Some(domain) = args.domain {
                    states.retain(|v| {
                        v.get("entity_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|id| id.starts_with(&format!("{domain}.")))
                    });
                }
                if !args.include_sensitive.unwrap_or(false) {
                    states.retain(|v| {
                        !v.get("entity_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|id| {
                                matches!(
                                    id.split('.').next(),
                                    Some(
                                        "camera"
                                            | "person"
                                            | "device_tracker"
                                            | "lock"
                                            | "alarm_control_panel"
                                    )
                                )
                            })
                    });
                }
                states.into()
            }
            "home_get_state" => {
                let a: StateArgs = serde_json::from_str(arguments)?;
                self.get(
                    a.account_id.as_deref(),
                    &format!("/api/states/{}", urlencoding::encode(&a.entity_id)),
                )
                .await?
            }
            "home_get_history" => {
                let a: HistoryArgs = serde_json::from_str(arguments)?;
                let (_, base, token) = self.auth(a.account_id.as_deref()).await?;
                let mut req = self
                    .client
                    .get(format!(
                        "{base}/api/history/period/{}",
                        urlencoding::encode(&a.start_time)
                    ))
                    .bearer_auth(token)
                    .query(&[("filter_entity_id", a.entity_id)]);
                if let Some(end) = a.end_time {
                    req = req.query(&[("end_time", end)]);
                }
                response_json(req.send().await?, "Home Assistant history").await?
            }
            "home_list_services" => {
                let a: AccountArgs = serde_json::from_str(arguments)?;
                self.get(a.account_id.as_deref(), "/api/services").await?
            }
            "home_call_service" => {
                let a: ServiceArgs = serde_json::from_str(arguments)?;
                anyhow::ensure!(
                    a.domain.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                        && a.service
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '_'),
                    "Invalid service name"
                );
                let (_, base, token) = self.auth(a.account_id.as_deref()).await?;
                response_json(
                    self.client
                        .post(format!("{base}/api/services/{}/{}", a.domain, a.service))
                        .bearer_auth(token)
                        .json(&a.service_data.unwrap_or_else(|| serde_json::json!({})))
                        .send()
                        .await?,
                    "Home Assistant service",
                )
                .await?
            }
            _ => anyhow::bail!("Unknown Home Assistant tool: {name}"),
        };
        Ok(serde_json::to_string_pretty(&value)?)
    }
    async fn poll(&self) -> anyhow::Result<()> {
        for account in self.db.list_integration_accounts("home_assistant").await? {
            if let Err(error) = self.poll_account(&account).await {
                tracing::warn!(
                    "Home Assistant poll failed for account {}: {error}",
                    account.id
                );
            }
        }
        Ok(())
    }
}
