use crate::errors::AppError;
use crate::state::{AppState, PendingGoogleOAuth};
use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
    response::{Html, IntoResponse},
};
use chrono::{Duration, Utc};
use jossie_core::integration::OnboardingStatus;
use jossie_integration_google::GoogleIntegration;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use url::Url;
use uuid::Uuid;

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
    Query(query): Query<GoogleSetupQuery>,
) -> Result<axum::response::Redirect, AppError> {
    let base_url = resolve_public_base_url(&state, &headers)?;
    let redirect_uri = format!("{base_url}/oauth/callback");

    let oauth_state = Uuid::new_v4().to_string();
    {
        let mut pending = state.pending_google_oauth.write().await;
        prune_expired_oauth_states(&mut pending);
        pending.insert(
            oauth_state.clone(),
            PendingGoogleOAuth {
                account_name: query
                    .account_name
                    .as_ref()
                    .map(|name| name.trim().to_string())
                    .filter(|name| !name.is_empty()),
                created_at: Utc::now(),
            },
        );
    }

    let url = GoogleIntegration::generate_auth_url(
        &state.google_config,
        &redirect_uri,
        Some(&oauth_state),
    );
    Ok(axum::response::Redirect::to(&url))
}

#[derive(Deserialize)]
pub struct GoogleSetupQuery {
    account_name: Option<String>,
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    error: Option<String>,
    state: Option<String>,
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

    let base_url = match resolve_public_base_url(&state, &headers) {
        Ok(base_url) => base_url,
        Err(e) => return Html(format!("<h1>Configuration Error</h1><p>{}</p>", e)),
    };
    let redirect_uri = format!("{base_url}/oauth/callback");

    let Some(oauth_state) = query
        .state
        .as_deref()
        .filter(|state| !state.trim().is_empty())
    else {
        return Html("<h1>OAuth Error</h1><p>Missing OAuth state.</p>".to_string());
    };

    let pending_state = {
        let mut pending = state.pending_google_oauth.write().await;
        prune_expired_oauth_states(&mut pending);
        pending.remove(oauth_state)
    };

    let Some(pending_state) = pending_state else {
        return Html("<h1>OAuth Error</h1><p>Invalid or expired OAuth state.</p>".to_string());
    };

    let account_name = pending_state.account_name;

    let account_id = Uuid::new_v4().to_string();
    let account_label =
        account_name.unwrap_or_else(|| format!("Google Account {}", &account_id[..8]));

    match GoogleIntegration::exchange_code(&state.google_config, &code, &redirect_uri).await {
        Ok(token) => {
            if let Err(e) = state
                .db
                .upsert_integration_account(
                    &account_id,
                    "google",
                    &account_label,
                    &serde_json::json!({
                        "refresh_token": token,
                        "email": "",
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
        }
        Err(e) => Html(format!("<h1>Exchange Error</h1><p>{}</p>", e)),
    }
}

const GOOGLE_OAUTH_STATE_TTL_MINUTES: i64 = 15;

fn prune_expired_oauth_states(pending: &mut std::collections::HashMap<String, PendingGoogleOAuth>) {
    let cutoff = Utc::now() - Duration::minutes(GOOGLE_OAUTH_STATE_TTL_MINUTES);
    pending.retain(|_, state| state.created_at >= cutoff);
}

fn resolve_public_base_url(state: &AppState, headers: &HeaderMap) -> Result<String, AppError> {
    if let Some(base_url) = state.public_base_url.as_deref() {
        return Ok(normalize_public_base_url(base_url)?);
    }

    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("server.public_base_url must be configured for OAuth"))?;
    let host_without_port = strip_port(host);
    if !is_local_host(host_without_port) {
        return Err(anyhow::anyhow!(
            "server.public_base_url must be configured for non-local OAuth setups"
        )
        .into());
    }

    normalize_public_base_url(&format!("http://{host}"))
}

fn normalize_public_base_url(base_url: &str) -> Result<String, AppError> {
    let url =
        Url::parse(base_url).map_err(|e| anyhow::anyhow!("Invalid server.public_base_url: {e}"))?;

    match url.scheme() {
        "https" => {}
        "http" if url.host_str().map(is_local_host).unwrap_or(false) => {}
        "http" => {
            return Err(anyhow::anyhow!(
                "server.public_base_url must use https unless it points to localhost"
            )
            .into());
        }
        _ => {
            return Err(anyhow::anyhow!("server.public_base_url must use http or https").into());
        }
    }

    if url.query().is_some() || url.fragment().is_some() {
        return Err(anyhow::anyhow!(
            "server.public_base_url must not include a query string or fragment"
        )
        .into());
    }

    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn strip_port(host: &str) -> &str {
    if let Some(stripped) = host.strip_prefix('[') {
        return stripped.split(']').next().unwrap_or(host);
    }

    if host.matches(':').count() == 1 {
        return host.split(':').next().unwrap_or(host);
    }

    host
}

fn is_local_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

#[cfg(test)]
mod tests {
    use super::{is_local_host, normalize_public_base_url, strip_port};

    #[test]
    fn normalize_public_base_url_accepts_local_http() {
        assert_eq!(
            normalize_public_base_url("http://localhost:3000/").unwrap(),
            "http://localhost:3000"
        );
    }

    #[test]
    fn normalize_public_base_url_rejects_remote_http() {
        assert!(normalize_public_base_url("http://example.com").is_err());
    }

    #[test]
    fn strip_port_handles_ipv4_and_ipv6() {
        assert_eq!(strip_port("localhost:3000"), "localhost");
        assert_eq!(strip_port("[::1]:3000"), "::1");
        assert_eq!(strip_port("example.com"), "example.com");
        assert!(is_local_host(strip_port("[::1]:3000")));
    }
}
