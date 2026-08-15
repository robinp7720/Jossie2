use crate::errors::AppError;
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, ts_rs::TS)]
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

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("password")
        || lower.contains("refresh_token")
        || lower.contains("access_token")
        || lower.contains("client_secret")
        || lower == "token"
        || lower.ends_with("_token")
        || lower.contains("api_key")
}

fn merge_account_config(
    existing: serde_json::Value,
    update: serde_json::Value,
) -> serde_json::Value {
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    let Some(update) = update.as_object() else {
        return serde_json::Value::Object(merged);
    };

    for (key, value) in update {
        let retain_existing_secret =
            is_secret_key(key) && value.as_str().is_some_and(|value| value.trim().is_empty());
        if !retain_existing_secret {
            merged.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(merged)
}

fn validate_account_config(
    integration: &str,
    config: &serde_json::Value,
    require_secret: bool,
) -> Result<(), AppError> {
    let config = config.as_object().ok_or_else(|| {
        AppError::bad_request(anyhow::anyhow!("Account configuration must be an object"))
    })?;
    let required: &[&str] = match integration {
        "email" => &["username", "imap_host", "smtp_host"],
        "google" => &[],
        _ => {
            return Err(AppError::bad_request(anyhow::anyhow!(
                "Unsupported integration type: {integration}"
            )));
        }
    };
    for key in required {
        if config
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(AppError::bad_request(anyhow::anyhow!("{key} is required")));
        }
    }
    if require_secret {
        let secret = if integration == "email" {
            "password"
        } else {
            "refresh_token"
        };
        if config
            .get(secret)
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(AppError::bad_request(anyhow::anyhow!(
                "{secret} is required when adding an account"
            )));
        }
    }
    Ok(())
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
    validate_account_config(&req.integration, &req.config, true)?;
    let id = state
        .db
        .add_integration_account(&req.integration, &req.name, &req.config)
        .await?;
    Ok(Json(id))
}

#[derive(Deserialize)]
pub struct UpdateAccountRequest {
    pub name: String,
    pub config: serde_json::Value,
}

pub async fn update_account(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAccountRequest>,
) -> Result<Json<()>, AppError> {
    let account = state
        .db
        .get_integration_account(&id)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Account not found")))?;
    let existing = serde_json::from_str(&account.data).map_err(|_| {
        AppError::bad_request(anyhow::anyhow!("Stored account configuration is invalid"))
    })?;
    let merged = merge_account_config(existing, req.config);
    validate_account_config(&account.integration, &merged, false)?;
    if !state
        .db
        .update_integration_account(&id, req.name.trim(), &merged)
        .await?
    {
        return Err(AppError::not_found(anyhow::anyhow!("Account not found")));
    }
    Ok(Json(()))
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
    use super::{merge_account_config, sanitize_account_details};

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

    #[test]
    fn empty_secret_updates_preserve_existing_credentials() {
        let merged = merge_account_config(
            serde_json::json!({"username": "me@example.com", "password": "secret"}),
            serde_json::json!({"username": "new@example.com", "password": ""}),
        );
        assert_eq!(merged["username"], "new@example.com");
        assert_eq!(merged["password"], "secret");
    }
}
