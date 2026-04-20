use crate::errors::AppError;
use crate::state::AppState;
use axum::{
    Json,
    extract::{Multipart, State},
};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize)]
pub struct UploadResponse {
    pub file_id: Uuid,
    pub name: String,
}

pub async fn upload_file(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, AppError> {
    let file_id = Uuid::new_v4();
    let mut name = String::new();
    let mut content_type = None;
    let mut data = Vec::new();

    while let Some(field) = multipart.next_field().await.map_err(anyhow::Error::from)? {
        let field_name = field.name().unwrap_or_default().to_string();
        if field_name == "file" {
            name = field.file_name().unwrap_or("unnamed").to_string();
            content_type = field.content_type().map(|s| s.to_string());
            data = field.bytes().await.map_err(anyhow::Error::from)?.to_vec();
        }
    }

    if data.is_empty() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "No file data provided"
        )));
    }

    let uploads_dir = "uploads";
    tokio::fs::create_dir_all(uploads_dir)
        .await
        .map_err(anyhow::Error::from)?;

    let file_path = format!("{}/{}", uploads_dir, file_id);
    tokio::fs::write(&file_path, &data)
        .await
        .map_err(anyhow::Error::from)?;

    state
        .db
        .save_file_record(
            &file_id,
            &name,
            content_type.as_deref(),
            data.len() as i64,
            &file_path,
            None, // Linked to message later
        )
        .await
        .map_err(anyhow::Error::from)?;

    Ok(Json(UploadResponse { file_id, name }))
}
