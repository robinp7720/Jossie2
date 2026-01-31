use std::sync::Arc;
use axum::{
    extract::{State, Path},
    Json,
};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::errors::AppError;

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

pub async fn list_accounts(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<AccountConfig>>, AppError> {
    let mut accounts = Vec::new();

    // Default Google account (stored as integration setting)
    if let Some(token) = state
        .db
        .get_integration_setting("google", "refresh_token")
        .await?
    {
        if !token.is_empty() {
            accounts.push(AccountConfig {
                id: "google-default".to_string(),
                integration: "google".to_string(),
                name: "Default Google Account".to_string(),
                details: serde_json::json!({
                    "configured": true,
                    "source": "oauth"
                }),
            });
        }
    }

    // List Google Accounts
    let google_accounts = state.db.list_integration_accounts("google").await?;
    for acc in google_accounts {
        accounts.push(AccountConfig {
            id: acc.id,
            integration: "google".to_string(),
            name: acc.name,
            details: serde_json::from_str(&acc.data).unwrap_or_default(),
        });
    }

    // List Email Accounts
    let email_accounts = state.db.list_integration_accounts("email").await?;
    for acc in email_accounts {
        accounts.push(AccountConfig {
            id: acc.id,
            integration: "email".to_string(),
            name: acc.name,
            details: serde_json::from_str(&acc.data).unwrap_or_default(),
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
        return Err(anyhow::anyhow!("Unsupported integration type: {}", req.integration).into());
    }

    // For email, we could validate fields here, but for now just pass through
    let id = state.db.add_integration_account(&req.integration, &req.name, &req.config).await?;
    Ok(Json(id))
}

pub async fn delete_account(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<()>, AppError> {
    state.db.delete_integration_account(&id).await?;
    Ok(Json(()))
}
