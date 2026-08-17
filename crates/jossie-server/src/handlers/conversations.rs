use crate::errors::AppError;
use crate::events::ServerEvent;
use crate::state::AppState;
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::Response,
};
use jossie_core::types::{Message, Role};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ListConversationsParams {
    q: Option<String>,
    view: Option<String>,
    limit: Option<usize>,
    before: Option<Uuid>,
}

pub async fn list_conversations(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListConversationsParams>,
) -> Result<Json<Vec<jossie_db::ConversationListItem>>, AppError> {
    let view = params.view.as_deref().unwrap_or("active");
    if !matches!(view, "active" | "archived" | "all") {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "view must be active, archived, or all"
        )));
    }
    if params.q.as_deref().is_some_and(|q| q.chars().count() > 200) {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "Search queries may be at most 200 characters"
        )));
    }
    Ok(Json(
        state
            .db
            .list_conversation_items(
                view,
                params.q.as_deref(),
                params.limit.unwrap_or(50),
                params.before,
            )
            .await?,
    ))
}

pub async fn create_conversation(
    State(state): State<Arc<AppState>>,
) -> Result<Json<jossie_core::types::Conversation>, AppError> {
    Ok(Json(state.db.create_conversation(None).await?))
}

#[derive(Deserialize)]
pub struct UpdateConversationRequest {
    title: Option<String>,
    archived: Option<bool>,
}

pub async fn update_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateConversationRequest>,
) -> Result<Json<jossie_core::types::Conversation>, AppError> {
    if request.title.is_none() && request.archived.is_none() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "Provide a title or archived state"
        )));
    }
    let title = request.title.as_deref().map(str::trim);
    if title.is_some_and(|title| title.is_empty() || title.chars().count() > 120) {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "Conversation titles must contain 1 to 120 characters"
        )));
    }
    if request.archived == Some(true) && state.active_conversations.read().await.contains(&id) {
        return Err(AppError::conflict(anyhow::anyhow!(
            "Stop the current run before archiving this conversation"
        )));
    }
    let conversation = state
        .db
        .update_conversation(id, title, request.archived)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Conversation not found")))?;
    state
        .publish_event(ServerEvent::ConversationUpdated {
            conversation_id: id,
            title: conversation.title.clone(),
            archived_at: conversation.archived_at.map(|value| value.to_rfc3339()),
            updated_at: conversation.updated_at.to_rfc3339(),
        })
        .await;
    Ok(Json(conversation))
}

#[derive(Deserialize)]
pub struct GetMessagesParams {
    limit: Option<usize>,
    before: Option<Uuid>,
    around: Option<Uuid>,
}

pub async fn get_messages(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<GetMessagesParams>,
) -> Result<Json<Vec<Message>>, AppError> {
    if state.db.get_conversation(id).await?.is_none() {
        return Err(AppError::not_found(anyhow::anyhow!(
            "Conversation not found"
        )));
    }
    if params.before.is_some() && params.around.is_some() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "before and around cannot be used together"
        )));
    }
    let limit = params.limit.unwrap_or(100).clamp(1, 200);
    let messages = if let Some(before) = params.before {
        state.db.get_messages_before(id, before, limit).await?
    } else if let Some(around) = params.around {
        state.db.get_messages_around(id, around, limit).await?
    } else {
        state.db.get_messages(id, Some(limit)).await?
    };
    Ok(Json(messages))
}

#[derive(Deserialize)]
pub struct ExportParams {
    format: Option<String>,
}

pub async fn export_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<ExportParams>,
) -> Result<Response, AppError> {
    let conversation = state
        .db
        .get_conversation(id)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Conversation not found")))?;
    let messages = state.db.get_messages(id, None).await?;
    let visible = messages
        .into_iter()
        .filter(|message| {
            matches!(message.role, Role::User | Role::Assistant)
                && !message.content.trim().is_empty()
        })
        .collect::<Vec<_>>();
    let format = params.format.as_deref().unwrap_or("markdown");
    let title = conversation
        .title
        .as_deref()
        .unwrap_or("Untitled conversation");
    let safe_name = title
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(80)
        .collect::<String>();
    let safe_name = if safe_name.is_empty() {
        "conversation"
    } else {
        &safe_name
    };

    let (body, content_type, extension) = match format {
        "markdown" => (
            render_markdown_export(title, &conversation, &visible).into_bytes(),
            "text/markdown; charset=utf-8",
            "md",
        ),
        "json" => {
            let export = ConversationExport {
                version: 1,
                conversation: &conversation,
                messages: visible.iter().map(ExportMessage::from).collect(),
            };
            (
                serde_json::to_vec_pretty(&export).map_err(anyhow::Error::from)?,
                "application/json",
                "json",
            )
        }
        _ => {
            return Err(AppError::bad_request(anyhow::anyhow!(
                "format must be markdown or json"
            )));
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{safe_name}.{extension}\""),
        )
        .body(Body::from(body))
        .map_err(|error| AppError::from(anyhow::Error::from(error)))
}

#[derive(Serialize)]
struct ConversationExport<'a> {
    version: u8,
    conversation: &'a jossie_core::types::Conversation,
    messages: Vec<ExportMessage<'a>>,
}

#[derive(Serialize)]
struct ExportMessage<'a> {
    id: Uuid,
    role: &'static str,
    content: &'a str,
    created_at: chrono::DateTime<chrono::Utc>,
    attachments: &'a [jossie_core::types::Attachment],
}

impl<'a> From<&'a Message> for ExportMessage<'a> {
    fn from(message: &'a Message) -> Self {
        Self {
            id: message.id,
            role: message.role.as_str(),
            content: &message.content,
            created_at: message.created_at,
            attachments: message.attachments.as_deref().unwrap_or_default(),
        }
    }
}

fn render_markdown_export(
    title: &str,
    conversation: &jossie_core::types::Conversation,
    messages: &[Message],
) -> String {
    let mut output = format!(
        "# {title}\n\nCreated: {}\n\n",
        conversation.created_at.to_rfc3339()
    );
    for message in messages {
        let author = if message.role == Role::User {
            "You"
        } else {
            "Jossie"
        };
        output.push_str(&format!(
            "## {author} · {}\n\n{}\n\n",
            message.created_at.to_rfc3339(),
            message.content
        ));
        if let Some(attachments) = message.attachments.as_deref() {
            for attachment in attachments {
                output.push_str(&format!(
                    "- Attachment: {} ({} bytes{})\n",
                    attachment.name,
                    attachment.size,
                    attachment
                        .mime_type
                        .as_deref()
                        .map(|mime| format!(", {mime}"))
                        .unwrap_or_default()
                ));
            }
            if !attachments.is_empty() {
                output.push('\n');
            }
        }
    }
    output
}

#[derive(Serialize)]
pub struct DeleteConversationResponse {
    conversation_id: Uuid,
    deleted: bool,
    deleted_files: usize,
}

pub async fn delete_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<DeleteConversationResponse>, AppError> {
    let conversation = state
        .db
        .get_conversation(id)
        .await?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("Conversation not found")))?;
    if conversation.archived_at.is_none() {
        return Err(AppError::conflict(anyhow::anyhow!(
            "Archive the conversation before permanently deleting it"
        )));
    }
    if state.active_conversations.read().await.contains(&id)
        || state.db.conversation_has_active_dependencies(id).await?
    {
        return Err(AppError::conflict(anyhow::anyhow!(
            "Cancel active work, approvals, goals, and schedules before deleting this conversation"
        )));
    }

    let files = state.db.conversation_delete_files(id).await?;
    let mut staged = Vec::new();
    for file in &files {
        let source = PathBuf::from(&file.path);
        if !tokio::fs::try_exists(&source)
            .await
            .map_err(anyhow::Error::from)?
        {
            continue;
        }
        let trash = source
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(".trash");
        tokio::fs::create_dir_all(&trash)
            .await
            .map_err(anyhow::Error::from)?;
        let target = trash.join(format!("{}-{}-{}", id, file.id, Uuid::new_v4()));
        if let Err(error) = tokio::fs::rename(&source, &target).await {
            restore_staged_files(&staged).await;
            return Err(AppError::from(anyhow::anyhow!(
                "Failed to stage attachment {} for deletion: {error}",
                file.id
            )));
        }
        staged.push((file.id, source, target));
    }
    let file_ids = files.iter().map(|file| file.id).collect::<Vec<_>>();
    let deleted_file_ids = match state.db.delete_conversation_data(id, &file_ids).await {
        Ok(Some(deleted_file_ids)) => deleted_file_ids,
        Ok(None) => {
            restore_staged_files(&staged).await;
            return Err(AppError::conflict(anyhow::anyhow!(
                "The conversation changed and can no longer be deleted"
            )));
        }
        Err(error) => {
            restore_staged_files(&staged).await;
            return Err(AppError::from(error));
        }
    };
    for (file_id, source, target) in &staged {
        if !deleted_file_ids.contains(file_id) {
            if let Err(error) = tokio::fs::rename(target, source).await {
                tracing::error!(
                    "Failed to restore retained attachment {}: {error}",
                    source.display()
                );
            }
        } else if let Err(error) = tokio::fs::remove_file(target).await {
            tracing::warn!(
                "Failed to remove staged deleted attachment {}: {error}",
                target.display()
            );
        }
    }
    state
        .publish_event(ServerEvent::ConversationDeleted {
            conversation_id: id,
        })
        .await;
    Ok(Json(DeleteConversationResponse {
        conversation_id: id,
        deleted: true,
        deleted_files: deleted_file_ids.len(),
    }))
}

async fn restore_staged_files(staged: &[(Uuid, PathBuf, PathBuf)]) {
    for (_, source, target) in staged.iter().rev() {
        if let Some(parent) = source.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if let Err(error) = tokio::fs::rename(target, source).await {
            tracing::error!(
                "Failed to restore staged attachment {}: {error}",
                source.display()
            );
        }
    }
}

#[derive(Serialize)]
pub struct CancelRunResponse {
    pub conversation_id: Uuid,
    pub status: &'static str,
}

pub async fn cancel_conversation_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<CancelRunResponse>, AppError> {
    state.request_cancel(id).await;
    Ok(Json(CancelRunResponse {
        conversation_id: id,
        status: "cancel_requested",
    }))
}
