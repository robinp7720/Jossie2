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

CREATE TABLE IF NOT EXISTS integration_events (
    id TEXT PRIMARY KEY,
    integration TEXT NOT NULL,
    account_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    dedupe_key TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'new',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    processed_at TEXT,
    last_error TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_integration_events_dedupe
    ON integration_events(integration, account_id, dedupe_key);
CREATE INDEX IF NOT EXISTS idx_integration_events_status
    ON integration_events(status, created_at);

-- Graph Nodes
CREATE TABLE IF NOT EXISTS graph_nodes (
    id TEXT PRIMARY KEY, -- Normalized label or UUID
    label TEXT NOT NULL,
    type TEXT NOT NULL, -- Person, Project, etc.
    properties TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Graph Edges
CREATE TABLE IF NOT EXISTS graph_edges (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
    target_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
    relation TEXT NOT NULL, -- WORKS_ON, CREATED, etc.
    weight REAL NOT NULL DEFAULT 1.0,
    properties TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_graph_edges_source ON graph_edges(source_id);
CREATE INDEX IF NOT EXISTS idx_graph_edges_target ON graph_edges(target_id);
