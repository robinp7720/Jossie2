use crate::events::ServerEvent;
use jossie_core::integration::IntegrationRegistry;
use jossie_db::Database;
use jossie_integration_google::GoogleIntegration;
use jossie_integration_mail::MailIntegration;
use jossie_llm::LlmClient;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub struct PendingGoogleOAuth {
    pub account_name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub struct AgentRuntimeConfig {
    pub system_prompt: String,
    pub max_agent_iterations: usize,
    pub max_context_messages: usize,
    pub event_max_context_messages: usize,
    pub openai_optimizations: bool,
    pub max_context_chars: usize,
    pub context_compact_target_chars: usize,
    pub context_keep_recent_dialogue_messages: usize,
    pub interactive_run_budget_seconds: u64,
    pub llm_request_timeout_seconds: u64,
    pub tool_call_timeout_seconds: u64,
    pub max_tool_result_chars: usize,
    pub max_tool_batch_chars: usize,
    pub max_attachment_bytes_per_request: usize,
    pub enable_self_reflection: bool,
}

pub struct WebRuntimeConfig {
    pub auth_token: String,
    pub auth_password_hash: String,
    pub session_cookie_secure: bool,
    pub public_base_url: Option<String>,
    pub cors_origins: Vec<String>,
    pub max_request_body_bytes: usize,
}

pub struct TelegramRuntimeConfig {
    pub token: String,
    pub max_download_bytes: usize,
    pub ffmpeg_path: String,
}

pub struct BackgroundRuntimeConfig {
    pub heartbeat_enabled: bool,
    pub heartbeat_interval_secs: u64,
}

pub struct AppState {
    pub db: Arc<Database>,
    pub llm: LlmClient,
    pub kg_llm: LlmClient,
    pub chat_export_importer: Arc<jossie_integration_files::ChatExportImporter>,
    pub registry: Arc<IntegrationRegistry>,
    pub mail_integration: Arc<MailIntegration>,
    pub agent: AgentRuntimeConfig,
    pub web: WebRuntimeConfig,
    pub telegram: TelegramRuntimeConfig,
    pub background: BackgroundRuntimeConfig,
    pub google_config: jossie_core::config::GoogleConfig,
    pub google_integration: Option<Arc<GoogleIntegration>>,
    pub active_conversations: Arc<RwLock<HashSet<Uuid>>>,
    pub cancelled_conversations: Arc<RwLock<HashSet<Uuid>>>,
    pub run_cancellations: Arc<RwLock<HashMap<Uuid, CancellationToken>>>,
    pub pending_google_oauth: Arc<RwLock<HashMap<String, PendingGoogleOAuth>>>,
    pub event_tx: broadcast::Sender<ServerEvent>,
}

impl AppState {
    pub async fn publish_durable_event(&self, event: ServerEvent) {
        crate::events::persist_activity_event(&self.db, &event).await;
        let work_events = crate::events::persist_work_event(&self.db, &event).await;
        let _ = self.event_tx.send(event);
        for work_event in work_events {
            let _ = self.event_tx.send(work_event);
        }
    }

    pub fn publish_event(&self, event: ServerEvent) {
        let activity_db = self.db.clone();
        let activity_event = event.clone();
        let work_db = self.db.clone();
        let work_event = event.clone();
        let work_tx = self.event_tx.clone();
        let _ = self.event_tx.send(event);
        tokio::spawn(async move {
            crate::events::persist_activity_event(&activity_db, &activity_event).await;
        });
        tokio::spawn(async move {
            for derived in crate::events::persist_work_event(&work_db, &work_event).await {
                let _ = work_tx.send(derived);
            }
        });
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<ServerEvent> {
        self.event_tx.subscribe()
    }

    pub async fn request_cancel(&self, conversation_id: Uuid) {
        self.cancelled_conversations
            .write()
            .await
            .insert(conversation_id);
        if let Some(token) = self.run_cancellations.read().await.get(&conversation_id) {
            token.cancel();
        }
        self.publish_event(ServerEvent::CancelRequested { conversation_id });
    }

    pub async fn clear_cancel(&self, conversation_id: Uuid) {
        self.cancelled_conversations
            .write()
            .await
            .remove(&conversation_id);
        self.run_cancellations
            .write()
            .await
            .remove(&conversation_id);
    }

    pub async fn is_cancel_requested(&self, conversation_id: Uuid) -> bool {
        self.cancelled_conversations
            .read()
            .await
            .contains(&conversation_id)
    }

    pub async fn begin_run_cancellation(&self, conversation_id: Uuid) -> CancellationToken {
        let token = CancellationToken::new();
        self.run_cancellations
            .write()
            .await
            .insert(conversation_id, token.clone());
        token
    }

    pub async fn run_cancellation(&self, conversation_id: Uuid) -> CancellationToken {
        self.run_cancellations
            .read()
            .await
            .get(&conversation_id)
            .cloned()
            .unwrap_or_else(CancellationToken::new)
    }
}
