use crate::events::ServerEvent;
use jossie_core::integration::IntegrationRegistry;
use jossie_db::Database;
use jossie_integration_google::GoogleIntegration;
use jossie_llm::LlmClient;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use uuid::Uuid;

pub struct PendingGoogleOAuth {
    pub account_name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub struct AppState {
    pub db: Arc<Database>,
    pub llm: LlmClient,
    pub kg_llm: LlmClient,
    pub chat_export_importer: Arc<jossie_integration_files::ChatExportImporter>,
    pub registry: Arc<IntegrationRegistry>,
    pub auth_token: String,
    pub auth_password_hash: String,
    pub session_cookie_secure: bool,
    pub public_base_url: Option<String>,
    pub system_prompt: String,
    pub max_agent_iterations: usize,
    pub max_context_messages: usize,
    pub event_max_context_messages: usize,
    pub openai_optimizations: bool,
    pub max_context_chars: usize,
    pub context_compact_target_chars: usize,
    pub context_keep_recent_dialogue_messages: usize,
    pub google_config: jossie_core::config::GoogleConfig,
    pub google_integration: Option<Arc<GoogleIntegration>>,
    pub telegram_token: String,
    pub enable_self_reflection: bool,
    pub heartbeat_enabled: bool,
    pub heartbeat_interval_secs: u64,
    pub active_conversations: Arc<RwLock<HashSet<Uuid>>>,
    pub cancelled_conversations: Arc<RwLock<HashSet<Uuid>>>,
    pub pending_google_oauth: Arc<RwLock<HashMap<String, PendingGoogleOAuth>>>,
    pub event_tx: broadcast::Sender<ServerEvent>,
    pub cors_origins: Vec<String>,
    pub max_request_body_bytes: usize,
}

impl AppState {
    pub fn publish_event(&self, event: ServerEvent) {
        let activity_db = self.db.clone();
        let activity_event = event.clone();
        let _ = self.event_tx.send(event);
        tokio::spawn(async move {
            crate::events::persist_activity_event(&activity_db, &activity_event).await;
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
        self.publish_event(ServerEvent::CancelRequested { conversation_id });
    }

    pub async fn clear_cancel(&self, conversation_id: Uuid) {
        self.cancelled_conversations
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
}
