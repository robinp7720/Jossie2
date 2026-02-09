use crate::errors::ErrorBody;
use crate::state::AppState;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

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

    match token {
        Some(t) if t == state.auth_token => Ok(next.run(request).await),
        _ => {
            let body = ErrorBody {
                error: "unauthorized".to_string(),
            };
            Err((StatusCode::UNAUTHORIZED, Json(body)).into_response())
        }
    }
}
