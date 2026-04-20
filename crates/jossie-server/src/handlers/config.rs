use crate::errors::AppError;
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize)]
pub struct AccountConfig {
    pub id: String,
    pub integration: String,
    pub name: String,
    pub details: serde_json::Value,
}

#[derive(Deserialize)]
pub struct AddAccountRequest {
    pub integration: String, // "google" or "email"
    pub name: String,
    pub config: serde_json::Value,
}

fn redact_secret_fields(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let redacted = map
                .into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let value = if lower.contains("password")
                        || lower.contains("refresh_token")
                        || lower.contains("access_token")
                        || lower.contains("client_secret")
                        || lower == "token"
                        || lower.ends_with("_token")
                        || lower.contains("api_key")
                    {
                        serde_json::Value::String("[REDACTED]".to_string())
                    } else {
                        redact_secret_fields(value)
                    };
                    (key, value)
                })
                .collect();
            serde_json::Value::Object(redacted)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(redact_secret_fields).collect())
        }
        other => other,
    }
}

fn sanitize_account_details(raw: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(raw)
        .map(redact_secret_fields)
        .unwrap_or_else(|_| serde_json::json!({}))
}

pub async fn list_accounts(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<AccountConfig>>, AppError> {
    let mut accounts = Vec::new();

    // List Google Accounts
    let google_accounts = state.db.list_integration_accounts("google").await?;
    for acc in google_accounts {
        accounts.push(AccountConfig {
            id: acc.id,
            integration: "google".to_string(),
            name: acc.name,
            details: sanitize_account_details(&acc.data),
        });
    }

    // List Email Accounts
    let email_accounts = state.db.list_integration_accounts("email").await?;
    for acc in email_accounts {
        accounts.push(AccountConfig {
            id: acc.id,
            integration: "email".to_string(),
            name: acc.name,
            details: sanitize_account_details(&acc.data),
        });
    }

    Ok(Json(accounts))
}

pub async fn add_account(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddAccountRequest>,
) -> Result<Json<String>, AppError> {
    // Basic validation
    if req.integration != "google" && req.integration != "email" {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "Unsupported integration type: {}",
            req.integration
        )));
    }

    // For email, we could validate fields here, but for now just pass through
    let id = state
        .db
        .add_integration_account(&req.integration, &req.name, &req.config)
        .await?;
    Ok(Json(id))
}

pub async fn delete_account(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<()>, AppError> {
    state.db.delete_integration_account(&id).await?;
    Ok(Json(()))
}

#[cfg(test)]
mod tests {
    use super::sanitize_account_details;

    #[test]
    fn sanitize_account_details_redacts_common_secrets() {
        let value = sanitize_account_details(
            r#"{
                "username":"me@example.com",
                "password":"secret",
                "nested":{"refresh_token":"abc","access_token":"def"},
                "items":[{"client_secret":"ghi"}]
            }"#,
        );

        assert_eq!(value["username"], "me@example.com");
        assert_eq!(value["password"], "[REDACTED]");
        assert_eq!(value["nested"]["refresh_token"], "[REDACTED]");
        assert_eq!(value["nested"]["access_token"], "[REDACTED]");
        assert_eq!(value["items"][0]["client_secret"], "[REDACTED]");
    }
}
