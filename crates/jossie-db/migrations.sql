CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    title TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    role TEXT NOT NULL,
    content TEXT NOT NULL DEFAULT '',
    tool_calls TEXT,
    tool_call_id TEXT,
    name TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_messages_conversation ON messages(conversation_id, created_at);

CREATE VIRTUAL TABLE IF NOT EXISTS memory USING fts5(
    key,
    content,
    tags
);

CREATE TABLE IF NOT EXISTS memory_metadata (
    key TEXT PRIMARY KEY,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS telegram_chats (
    telegram_chat_id INTEGER PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS integration_settings (
    integration TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (integration, key)
);

CREATE TABLE IF NOT EXISTS integration_accounts (
    id TEXT PRIMARY KEY,
    integration TEXT NOT NULL,
    name TEXT NOT NULL,
    data TEXT NOT NULL, -- JSON configuration
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);