use std::sync::Arc;
use axum::{
    extract::{State, Query},
    response::{IntoResponse, Html},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use jossie_core::integration::OnboardingStatus;
use jossie_integration_google::GoogleIntegration;
use crate::state::AppState;
use crate::errors::AppError;

#[derive(Serialize)]
pub struct IntegrationStatus {
    name: String,
    #[serde(flatten)]
    status: OnboardingStatus,
}

pub async fn onboarding_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<IntegrationStatus>>, AppError> {
    let mut statuses = Vec::new();
    for integration in state.registry.get_integrations() {
        let status = integration.check_onboarding().await?;
        statuses.push(IntegrationStatus {
            name: integration.name().to_string(),
            status,
        });
    }
    Ok(Json(statuses))
}

pub async fn setup_google_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> axum::response::Redirect {
    let host = headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or("localhost:3000");
    let redirect_uri = format!("http://{}/oauth/callback", host);
    
    let url = GoogleIntegration::generate_auth_url(&state.google_config, &redirect_uri);
    axum::response::Redirect::to(&url)
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    error: Option<String>,
}

pub async fn oauth_callback_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> impl IntoResponse {
    if let Some(error) = query.error {
        return Html(format!("<h1>Google Auth Error</h1><p>{}</p>", error));
    }
    
    let Some(code) = query.code else {
        return Html("<h1>Error</h1><p>No code received.</p>".to_string());
    };

    let host = headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or("localhost:3000");
    let redirect_uri = format!("http://{}/oauth/callback", host);

    match GoogleIntegration::exchange_code(&state.google_config, &code, &redirect_uri).await {
        Ok(token) => {
            if let Err(e) = state.db.set_integration_setting("google", "refresh_token", &token).await {
                return Html(format!("<h1>Error Saving Token</h1><p>{}</p>", e));
            }

            if let Err(e) = state
                .db
                .upsert_integration_account(
                    "google-default",
                    "google",
                    "Default Google Account",
                    &serde_json::json!({
                        "configured": true,
                        "source": "oauth"
                    }),
                )
                .await
            {
                return Html(format!("<h1>Error Saving Account</h1><p>{}</p>", e));
            }
            Html(format!(
                r#"
                <h1>Success!</h1>
                <p>Google integration configured successfully.</p>
                <p>You can close this window.</p>
                <script>setTimeout(() => window.close(), 3000);</script>
                "#
            ))
        },
        Err(e) => Html(format!("<h1>Exchange Error</h1><p>{}</p>", e)),
    }
}
