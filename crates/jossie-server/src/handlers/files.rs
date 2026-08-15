use crate::errors::AppError;
use crate::state::AppState;
use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path, State},
    http::{StatusCode, header},
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize)]
pub struct UploadResponse {
    pub file_id: Uuid,
    pub name: String,
}

pub async fn download_file(
    State(state): State<Arc<AppState>>,
    Path(file_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let record = state
        .db
        .get_file_record(&file_id)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("File not found")))?;
    let data = tokio::fs::read(&record.path)
        .await
        .map_err(anyhow::Error::from)?;
    let filename = record
        .name
        .chars()
        .map(|character| {
            if character.is_ascii_graphic() && !matches!(character, '"' | '\\' | ';') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            record
                .mime_type
                .as_deref()
                .unwrap_or("application/octet-stream"),
        )
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(data))
        .map_err(|error| AppError::from(anyhow::Error::from(error)))
}

pub async fn delete_file(
    State(state): State<Arc<AppState>>,
    Path(file_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let record = state
        .db
        .get_unattached_file_record(&file_id)
        .await
        .map_err(AppError::conflict)?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("File not found")))?;

    let source = std::path::PathBuf::from(&record.path);
    let staged = if tokio::fs::try_exists(&source)
        .await
        .map_err(anyhow::Error::from)?
    {
        let trash = source
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(".trash");
        tokio::fs::create_dir_all(&trash)
            .await
            .map_err(anyhow::Error::from)?;
        let target = trash.join(format!("{}-{}", file_id, Uuid::new_v4()));
        tokio::fs::rename(&source, &target)
            .await
            .map_err(anyhow::Error::from)?;
        Some(target)
    } else {
        None
    };

    let deleted = state.db.delete_file_record_if_unattached(&file_id).await;
    if !matches!(deleted, Ok(true)) {
        if let Some(target) = staged.as_ref()
            && let Err(restore_error) = tokio::fs::rename(target, &source).await
        {
            tracing::error!(
                %file_id,
                "Failed to restore staged upload after database error: {restore_error}"
            );
        }
        return match deleted {
            Ok(false) => Err(AppError::conflict(anyhow::anyhow!(
                "File became attached before it could be deleted"
            ))),
            Err(error) => Err(AppError::from(error)),
            Ok(true) => unreachable!(),
        };
    }
    if let Some(target) = staged
        && let Err(error) = tokio::fs::remove_file(&target).await
    {
        tracing::warn!(
            %file_id,
            "Failed to remove staged upload {}: {error}",
            target.display()
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct StartChatImportRequest {
    pub file_id: Uuid,
    #[serde(default)]
    pub format: jossie_integration_files::ChatExportFormat,
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
        .await?;

    Ok(Json(UploadResponse { file_id, name }))
}

pub async fn start_chat_import(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartChatImportRequest>,
) -> Result<Json<jossie_db::ChatImport>, AppError> {
    let import = state
        .chat_export_importer
        .enqueue(request.file_id, request.format)
        .await
        .map_err(AppError::bad_request)?;
    Ok(Json(import))
}

pub async fn get_chat_import(
    State(state): State<Arc<AppState>>,
    Path(import_id): Path<String>,
) -> Result<Json<jossie_db::ChatImport>, AppError> {
    let import = state
        .db
        .get_chat_import(&import_id)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Chat import not found")))?;
    Ok(Json(import))
}
