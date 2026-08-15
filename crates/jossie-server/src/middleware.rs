use crate::errors::ErrorBody;
use crate::handlers::auth::{hash_session_token, session_token_from_headers};
use crate::state::AppState;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, Response> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| {
            request.uri().query().and_then(|q| {
                q.split('&').find_map(|pair| {
                    let mut split = pair.split('=');
                    if split.next() == Some("token") {
                        split.next()
                    } else {
                        None
                    }
                })
            })
        });

    let bearer_valid = token.is_some_and(|token| {
        token
            .as_bytes()
            .ct_eq(state.web.auth_token.as_bytes())
            .into()
    });
    let session_valid = if bearer_valid {
        false
    } else if let Some(session) = session_token_from_headers(&headers) {
        state
            .db
            .has_valid_auth_session(&hash_session_token(&session))
            .await
            .unwrap_or(false)
    } else {
        false
    };

    if bearer_valid || session_valid {
        Ok(next.run(request).await)
    } else {
        let body = ErrorBody {
            error: "unauthorized".to_string(),
        };
        Err((StatusCode::UNAUTHORIZED, Json(body)).into_response())
    }
}
