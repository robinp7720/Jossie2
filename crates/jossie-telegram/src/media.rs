#[derive(Clone, Copy, PartialEq, Eq)]
enum MediaKind {
    ModelAttachment,
    Voice,
    Audio,
}

struct LocalMedia {
    id: Uuid,
    name: String,
    mime_type: String,
    size: usize,
    path: PathBuf,
    kind: MediaKind,
}

struct MediaCandidate {
    file_id: FileId,
    name: String,
    mime_type: String,
    size: usize,
    kind: MediaKind,
}

async fn download_media_group(
    bot: &Bot,
    state: &AppState,
    messages: &[teloxide::types::Message],
) -> anyhow::Result<Vec<LocalMedia>> {
    let mut candidates = Vec::new();
    for message in messages {
        if let Some(photos) = message.photo()
            && let Some(photo) = photos.iter().max_by_key(|photo| photo.width * photo.height)
        {
            candidates.push(MediaCandidate {
                file_id: photo.file.id.clone(),
                name: format!("telegram-photo-{}.jpg", message.id.0),
                mime_type: "image/jpeg".to_string(),
                size: photo.file.size as usize,
                kind: MediaKind::ModelAttachment,
            });
            continue;
        }
        if let Some(document) = message.document() {
            let name = document
                .file_name
                .clone()
                .unwrap_or_else(|| format!("telegram-document-{}", message.id.0));
            let mime_type = document
                .mime_type
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "application/octet-stream".to_string());
            if !supported_document(&name, &mime_type) {
                anyhow::bail!("Unsupported Telegram document: {name}");
            }
            candidates.push(MediaCandidate {
                file_id: document.file.id.clone(),
                name,
                mime_type,
                size: document.file.size as usize,
                kind: MediaKind::ModelAttachment,
            });
            continue;
        }
        if let Some(voice) = message.voice() {
            ensure_voice_available(state).await?;
            candidates.push(MediaCandidate {
                file_id: voice.file.id.clone(),
                name: format!("telegram-voice-{}.ogg", message.id.0),
                mime_type: voice
                    .mime_type
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "audio/ogg".to_string()),
                size: voice.file.size as usize,
                kind: MediaKind::Voice,
            });
            continue;
        }
        if let Some(audio) = message.audio() {
            ensure_transcription_enabled(state)?;
            candidates.push(MediaCandidate {
                file_id: audio.file.id.clone(),
                name: audio
                    .file_name
                    .clone()
                    .unwrap_or_else(|| format!("telegram-audio-{}", message.id.0)),
                mime_type: audio
                    .mime_type
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "audio/mpeg".to_string()),
                size: audio.file.size as usize,
                kind: MediaKind::Audio,
            });
        }
    }
    let total_size = candidates
        .iter()
        .map(|candidate| candidate.size)
        .sum::<usize>();
    if total_size > state.telegram_max_download_bytes {
        anyhow::bail!(
            "Telegram media is too large: {total_size} bytes exceeds {}",
            state.telegram_max_download_bytes
        );
    }
    tokio::fs::create_dir_all("uploads").await?;
    let mut downloaded = Vec::new();
    for candidate in candidates {
        match download_candidate(bot, state, candidate).await {
            Ok(media) => {
                let total = downloaded
                    .iter()
                    .map(|item: &LocalMedia| item.size)
                    .sum::<usize>()
                    + media.size;
                if total > state.telegram_max_download_bytes {
                    let _ = tokio::fs::remove_file(&media.path).await;
                    cleanup_paths(
                        downloaded
                            .iter()
                            .map(|item: &LocalMedia| item.path.as_path()),
                    )
                    .await;
                    anyhow::bail!("Downloaded Telegram album exceeds the configured limit");
                }
                downloaded.push(media);
            }
            Err(error) => {
                cleanup_paths(
                    downloaded
                        .iter()
                        .map(|media: &LocalMedia| media.path.as_path()),
                )
                .await;
                return Err(error);
            }
        }
    }
    Ok(downloaded)
}

async fn download_candidate(
    bot: &Bot,
    state: &AppState,
    candidate: MediaCandidate,
) -> anyhow::Result<LocalMedia> {
    let id = Uuid::new_v4();
    let path = PathBuf::from("uploads").join(id.to_string());
    let result = async {
        let file = bot.get_file(candidate.file_id).await?;
        let mut destination = tokio::fs::File::create(&path).await?;
        bot.download_file(&file.path, &mut destination).await?;
        drop(destination);
        let actual_size = tokio::fs::metadata(&path).await?.len() as usize;
        if actual_size > state.telegram_max_download_bytes {
            anyhow::bail!("Downloaded Telegram file exceeds the configured limit");
        }
        Ok(LocalMedia {
            id,
            name: candidate.name,
            mime_type: candidate.mime_type,
            size: actual_size,
            path: path.clone(),
            kind: candidate.kind,
        })
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
    }
    result
}

async fn build_user_content(
    state: &AppState,
    messages: &[teloxide::types::Message],
    media: &[LocalMedia],
) -> anyhow::Result<String> {
    let caption = messages
        .iter()
        .find_map(|message| message.caption().or_else(|| message.text()))
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut transcripts = Vec::new();
    for item in media
        .iter()
        .filter(|item| matches!(item.kind, MediaKind::Voice | MediaKind::Audio))
    {
        let (path, filename, mime, temporary) =
            prepare_audio_for_transcription(state, item).await?;
        let transcript = state.llm.transcribe_file(&path, &filename, &mime).await;
        if temporary {
            let _ = tokio::fs::remove_file(&path).await;
        }
        transcripts.push(transcript?);
    }
    if !transcripts.is_empty() {
        let transcript = transcripts.join("\n\n");
        return Ok(if caption.is_empty() {
            transcript
        } else {
            format!("{caption}\n\nVoice transcript:\n{transcript}")
        });
    }
    if !caption.is_empty() {
        return Ok(caption);
    }
    if media
        .iter()
        .any(|item| item.mime_type.starts_with("image/"))
    {
        Ok("Please inspect the attached image or images and respond appropriately.".to_string())
    } else if !media.is_empty() {
        Ok("Please inspect and briefly summarize the attached document or documents.".to_string())
    } else {
        Ok(String::new())
    }
}

async fn prepare_audio_for_transcription(
    state: &AppState,
    media: &LocalMedia,
) -> anyhow::Result<(PathBuf, String, String, bool)> {
    let extension = Path::new(&media.name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "mp3" | "mp4" | "mpeg" | "mpga" | "m4a" | "wav" | "webm"
    ) {
        return Ok((
            media.path.clone(),
            media.name.clone(),
            media.mime_type.clone(),
            false,
        ));
    }
    let output = PathBuf::from("uploads").join(format!("transcode-{}.webm", Uuid::new_v4()));
    let status = tokio::process::Command::new(&state.telegram_ffmpeg_path)
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(&media.path)
        .args(["-c:a", "libopus", "-b:a", "64k"])
        .arg(&output)
        .status()
        .await?;
    if !status.success() {
        let _ = tokio::fs::remove_file(&output).await;
        anyhow::bail!("FFmpeg could not transcode the Telegram audio file");
    }
    Ok((
        output,
        format!("{}.webm", media.id),
        "audio/webm".to_string(),
        true,
    ))
}

async fn ensure_voice_available(state: &AppState) -> anyhow::Result<()> {
    ensure_transcription_enabled(state)?;
    let output = tokio::process::Command::new(&state.telegram_ffmpeg_path)
        .arg("-version")
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!("FFmpeg is unavailable");
    }
    Ok(())
}

fn ensure_transcription_enabled(state: &AppState) -> anyhow::Result<()> {
    if !state.llm.transcription_is_configured() {
        anyhow::bail!("Voice transcription is disabled");
    }
    Ok(())
}

async fn persist_media_message(
    state: &AppState,
    message: &JossieMessage,
    media: &[LocalMedia],
) -> anyhow::Result<()> {
    let mut saved = Vec::new();
    for item in media {
        if let Err(error) = state
            .db
            .save_file_record(
                &item.id,
                &item.name,
                Some(&item.mime_type),
                item.size as i64,
                item.path.to_string_lossy().as_ref(),
                Some(message.conversation_id),
            )
            .await
        {
            for id in saved {
                let _ = state.db.delete_file_record(&id).await;
            }
            return Err(error);
        }
        saved.push(item.id);
    }
    jossie_server::events::persist_message(state, message).await?;
    for item in media {
        state
            .db
            .link_message_attachment(message.id, item.id)
            .await?;
    }
    Ok(())
}

async fn cleanup_local_media(state: &AppState, media: &[LocalMedia]) {
    for item in media {
        let _ = state.db.delete_file_record(&item.id).await;
        let _ = tokio::fs::remove_file(&item.path).await;
    }
}

async fn cleanup_paths<'a>(paths: impl Iterator<Item = &'a Path>) {
    for path in paths {
        let _ = tokio::fs::remove_file(path).await;
    }
}

fn supported_document(name: &str, mime: &str) -> bool {
    if matches!(
        mime,
        "application/pdf" | "image/jpeg" | "image/png" | "image/webp" | "image/gif"
    ) || mime.starts_with("text/")
    {
        return true;
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "pdf"
            | "jpg"
            | "jpeg"
            | "png"
            | "webp"
            | "gif"
            | "txt"
            | "md"
            | "json"
            | "html"
            | "xml"
            | "yaml"
            | "yml"
            | "csv"
            | "tsv"
            | "doc"
            | "docx"
            | "rtf"
            | "odt"
            | "ppt"
            | "pptx"
            | "xls"
            | "xlsx"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "java"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "go"
            | "rb"
            | "php"
            | "sh"
            | "sql"
            | "toml"
            | "css"
    )
}
