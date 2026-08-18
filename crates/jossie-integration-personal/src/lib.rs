mod home;
mod notion;
mod spotify;
mod tasks;

pub use home::HomeIntegration;
pub use notion::NotionIntegration;
pub use spotify::SpotifyIntegration;
pub use tasks::TasksIntegration;

use jossie_core::integration::{ConnectionField, ConnectionSpec};

fn token_field() -> ConnectionField {
    ConnectionField {
        name: "access_token".into(),
        label: "Access token".into(),
        input_type: "password".into(),
        required: true,
        secret: true,
        description: Some("Stored locally and never returned by the API.".into()),
        default_value: None,
    }
}

fn spec(
    integration: &str,
    display_name: &str,
    description: &str,
    fields: Vec<ConnectionField>,
    oauth_available: bool,
) -> ConnectionSpec {
    ConnectionSpec {
        integration: integration.into(),
        display_name: display_name.into(),
        description: description.into(),
        fields,
        oauth_available,
    }
}

async fn response_json(
    response: reqwest::Response,
    operation: &str,
) -> anyhow::Result<serde_json::Value> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("{operation} failed ({status}): {body}");
    }
    if body.trim().is_empty() {
        return Ok(serde_json::json!({"ok": true}));
    }
    Ok(serde_json::from_str(&body)?)
}

fn account_data(account: &jossie_db::IntegrationAccount) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::from_str(&account.data)?)
}
