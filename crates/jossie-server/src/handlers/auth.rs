use crate::errors::AppError;
use crate::state::AppState;
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

pub const SESSION_COOKIE_NAME: &str = "jossie_session";
const SESSION_TTL_SECONDS: i64 = 60 * 60 * 24 * 30;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub authenticated: bool,
}

pub fn hash_session_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

pub fn session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == SESSION_COOKIE_NAME).then(|| value.to_string())
    })
}

fn session_cookie(value: &str, secure: bool, max_age: i64) -> String {
    let mut cookie =
        format!("{SESSION_COOKIE_NAME}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

pub async fn login_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LoginRequest>,
) -> Result<Response, AppError> {
    if state.web.auth_password_hash.trim().is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            anyhow::anyhow!("Password login is not configured"),
        ));
    }

    let valid = PasswordHash::new(&state.web.auth_password_hash)
        .ok()
        .and_then(|hash| {
            Argon2::default()
                .verify_password(request.password.as_bytes(), &hash)
                .ok()
        })
        .is_some();
    if !valid {
        return Err(AppError::new(
            StatusCode::UNAUTHORIZED,
            anyhow::anyhow!("Invalid password"),
        ));
    }

    state.db.prune_expired_auth_sessions().await?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    state
        .db
        .create_auth_session(&hash_session_token(&token))
        .await?;

    let mut response = Json(SessionResponse {
        authenticated: true,
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&token, state.web.session_cookie_secure, SESSION_TTL_SECONDS)
            .parse()
            .expect("session cookie is a valid header value"),
    );
    Ok(response)
}

pub async fn session_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Json<SessionResponse> {
    let authenticated = match session_token_from_headers(&headers) {
        Some(token) => state
            .db
            .has_valid_auth_session(&hash_session_token(&token))
            .await
            .unwrap_or(false),
        None => false,
    };
    Json(SessionResponse { authenticated })
}

pub async fn logout_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if let Some(token) = session_token_from_headers(&headers) {
        state
            .db
            .revoke_auth_session(&hash_session_token(&token))
            .await?;
    }
    let mut response = Json(SessionResponse {
        authenticated: false,
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie("", state.web.session_cookie_secure, 0)
            .parse()
            .expect("session cookie is a valid header value"),
    );
    Ok(response)
}
