use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityGroup {
    Core,
    Memory,
    Knowledge,
    Files,
    Mail,
    Calendar,
    Drive,
    Web,
    Scheduler,
    Tasks,
    Contacts,
    Home,
    Notes,
    Media,
}

impl CapabilityGroup {
    pub const ACTIVATABLE: [Self; 12] = [
        Self::Knowledge,
        Self::Files,
        Self::Mail,
        Self::Calendar,
        Self::Drive,
        Self::Web,
        Self::Scheduler,
        Self::Tasks,
        Self::Contacts,
        Self::Home,
        Self::Notes,
        Self::Media,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Memory => "memory",
            Self::Knowledge => "knowledge",
            Self::Files => "files",
            Self::Mail => "mail",
            Self::Calendar => "calendar",
            Self::Drive => "drive",
            Self::Web => "web",
            Self::Scheduler => "scheduler",
            Self::Tasks => "tasks",
            Self::Contacts => "contacts",
            Self::Home => "home",
            Self::Notes => "notes",
            Self::Media => "media",
        }
    }
}

impl std::str::FromStr for CapabilityGroup {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "core" => Ok(Self::Core),
            "memory" => Ok(Self::Memory),
            "knowledge" => Ok(Self::Knowledge),
            "files" => Ok(Self::Files),
            "mail" | "email" => Ok(Self::Mail),
            "calendar" => Ok(Self::Calendar),
            "drive" => Ok(Self::Drive),
            "web" | "browser" => Ok(Self::Web),
            "scheduler" => Ok(Self::Scheduler),
            "tasks" => Ok(Self::Tasks),
            "contacts" => Ok(Self::Contacts),
            "home" | "smart_home" | "home_assistant" => Ok(Self::Home),
            "notes" => Ok(Self::Notes),
            "media" => Ok(Self::Media),
            _ => Err(format!("Unknown capability group: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    Read,
    LocalWrite,
    ExternalWrite,
    Destructive,
}

impl ToolEffect {
    const fn requires_approval_by_default(self) -> bool {
        matches!(self, Self::ExternalWrite | Self::Destructive)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolMetadata {
    pub capability: CapabilityGroup,
    pub effect: ToolEffect,
    pub requires_approval: bool,
    pub concurrent: bool,
    pub retry_transient: bool,
}

impl ToolMetadata {
    const fn read(capability: CapabilityGroup) -> Self {
        Self {
            capability,
            effect: ToolEffect::Read,
            requires_approval: false,
            concurrent: true,
            retry_transient: true,
        }
    }

    const fn local_write(capability: CapabilityGroup) -> Self {
        Self {
            capability,
            effect: ToolEffect::LocalWrite,
            requires_approval: false,
            concurrent: false,
            retry_transient: false,
        }
    }

    const fn action(capability: CapabilityGroup, effect: ToolEffect) -> Self {
        Self {
            capability,
            effect,
            requires_approval: effect.requires_approval_by_default(),
            concurrent: false,
            retry_transient: false,
        }
    }

    const fn action_without_approval(capability: CapabilityGroup, effect: ToolEffect) -> Self {
        Self {
            capability,
            effect,
            requires_approval: false,
            concurrent: false,
            retry_transient: false,
        }
    }
}

/// Central policy for the current built-in tool set. Unknown tools fail safe as
/// serial external writes until their policy is added here.
pub fn tool_metadata(tool_name: &str, arguments: &str) -> ToolMetadata {
    let read = match tool_name {
        "memory_get"
        | "memory_generate_totp"
        | "memory_search"
        | "memory_list_keys"
        | "memory_list_all" => Some(CapabilityGroup::Memory),
        "graph_search" | "graph_list_by_type" | "graph_explore_connections" => {
            Some(CapabilityGroup::Knowledge)
        }
        "list_files" | "read_file" => Some(CapabilityGroup::Files),
        "mail_list_accounts" | "mail_search" | "mail_read" | "mail_list_mailboxes" => {
            Some(CapabilityGroup::Mail)
        }
        "google_list_accounts" | "calendar_list_calendars" | "calendar_list_events" => {
            Some(CapabilityGroup::Calendar)
        }
        "drive_search" | "drive_read" | "drive_list_files" => Some(CapabilityGroup::Drive),
        "browser_read_page"
        | "browser_session_snapshot"
        | "browser_navigate"
        | "browser_search" => Some(CapabilityGroup::Web),
        "list_scheduled_tasks" => Some(CapabilityGroup::Scheduler),
        "task_list_accounts" | "task_list_projects" | "task_list" => Some(CapabilityGroup::Tasks),
        "contacts_search" | "contacts_read" => Some(CapabilityGroup::Contacts),
        "home_list_entities" | "home_get_state" | "home_get_history" | "home_list_services" => {
            Some(CapabilityGroup::Home)
        }
        "notes_search" | "notes_read" => Some(CapabilityGroup::Notes),
        "media_search" | "media_now_playing" | "media_get_queue" => Some(CapabilityGroup::Media),
        _ => None,
    };
    if let Some(capability) = read {
        return ToolMetadata::read(capability);
    }

    match tool_name {
        "memory_save" => ToolMetadata::local_write(CapabilityGroup::Memory),
        "graph_upsert_node" | "graph_add_relation" => {
            ToolMetadata::local_write(CapabilityGroup::Knowledge)
        }
        "ingest_chat_export" => ToolMetadata::local_write(CapabilityGroup::Files),
        "browser_open_session" | "browser_close_session" => {
            ToolMetadata::local_write(CapabilityGroup::Web)
        }
        "memory_delete" => ToolMetadata::local_write(CapabilityGroup::Memory),
        "graph_delete_node" | "graph_delete_relation" => {
            ToolMetadata::local_write(CapabilityGroup::Knowledge)
        }
        "mail_send" => ToolMetadata::action(CapabilityGroup::Mail, ToolEffect::ExternalWrite),
        "calendar_create_event" | "calendar_update_event" => {
            ToolMetadata::action(CapabilityGroup::Calendar, ToolEffect::ExternalWrite)
        }
        "browser_fill_input" | "browser_click" | "browser_select_option" => {
            ToolMetadata::action(CapabilityGroup::Web, ToolEffect::ExternalWrite)
        }
        "schedule_task"
        | "schedule_recurring_task"
        | "schedule_cron_task"
        | "send_user_message" => {
            ToolMetadata::action(CapabilityGroup::Scheduler, ToolEffect::ExternalWrite)
        }
        "cancel_scheduled_task" => {
            ToolMetadata::action(CapabilityGroup::Scheduler, ToolEffect::Destructive)
        }
        "task_create" | "task_update" | "task_complete" => {
            ToolMetadata::action(CapabilityGroup::Tasks, ToolEffect::ExternalWrite)
        }
        "home_call_service" => {
            ToolMetadata::action_without_approval(CapabilityGroup::Home, ToolEffect::ExternalWrite)
        }
        "notes_create_page" | "notes_append" => {
            ToolMetadata::action(CapabilityGroup::Notes, ToolEffect::ExternalWrite)
        }
        "media_play" | "media_pause" | "media_add_to_queue" | "media_create_playlist" => {
            ToolMetadata::action(CapabilityGroup::Media, ToolEffect::ExternalWrite)
        }
        "http_request" => {
            let method = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|value| value.get("method")?.as_str().map(str::to_uppercase))
                .unwrap_or_else(|| "GET".to_string());
            if matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS") {
                ToolMetadata::read(CapabilityGroup::Web)
            } else if method == "DELETE" {
                ToolMetadata::action(CapabilityGroup::Web, ToolEffect::Destructive)
            } else {
                ToolMetadata::action(CapabilityGroup::Web, ToolEffect::ExternalWrite)
            }
        }
        _ => ToolMetadata::action(CapabilityGroup::Core, ToolEffect::ExternalWrite),
    }
}

// --- Tool Result Validation (#2) ---
