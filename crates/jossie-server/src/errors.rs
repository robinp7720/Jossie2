use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Serialize)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    error: anyhow::Error,
}

impl AppError {
    pub fn new(status: StatusCode, error: impl Into<anyhow::Error>) -> Self {
        Self {
            status,
            error: error.into(),
        }
    }

    pub fn bad_request(error: impl Into<anyhow::Error>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error)
    }

    pub fn not_found(error: impl Into<anyhow::Error>) -> Self {
        Self::new(StatusCode::NOT_FOUND, error)
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!("{:#}", self.error);
        } else {
            tracing::warn!("{}: {}", self.status, self.error);
        }
        let body = ErrorBody {
            error: self.error.to_string(),
        };
        (self.status, Json(body)).into_response()
    }
}
