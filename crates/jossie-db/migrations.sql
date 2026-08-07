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
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    prompt_scope TEXT NOT NULL DEFAULT 'none',
    importance INTEGER NOT NULL DEFAULT 0
);

ALTER TABLE memory_metadata ADD COLUMN prompt_scope TEXT NOT NULL DEFAULT 'none';
ALTER TABLE memory_metadata ADD COLUMN importance INTEGER NOT NULL DEFAULT 0;

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
CREATE INDEX IF NOT EXISTS idx_graph_nodes_type_updated
    ON graph_nodes(type, updated_at DESC);

-- Scheduled tasks created by the agent via chat tools
CREATE TABLE IF NOT EXISTS scheduled_tasks (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    task_type TEXT NOT NULL,  -- 'agent_run', 'tool_call', etc.
    task_data TEXT NOT NULL,  -- JSON payload
    schedule_type TEXT NOT NULL,  -- 'once', 'interval', 'cron'
    schedule_value TEXT NOT NULL, -- ISO timestamp, interval seconds, or cron expression
    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'running', 'completed', 'failed', 'cancelled'
    next_run_at TEXT,
    last_run_at TEXT,
    run_count INTEGER NOT NULL DEFAULT 0,
    max_runs INTEGER,  -- NULL for infinite
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_error TEXT
);

CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_next_run 
    ON scheduled_tasks(status, next_run_at);

-- Out-of-band messages queued by the agent
CREATE TABLE IF NOT EXISTS out_of_band_messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    sender TEXT NOT NULL DEFAULT 'assistant',  -- 'assistant', 'system', etc.
    content TEXT NOT NULL,
    priority TEXT NOT NULL DEFAULT 'normal',  -- 'low', 'normal', 'high', 'urgent'
    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'sent', 'failed'
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    sent_at TEXT,
    last_error TEXT
);

CREATE INDEX IF NOT EXISTS idx_oob_messages_status
    ON out_of_band_messages(status, created_at);

-- Browser login sessions. Only a SHA-256 digest of the opaque cookie value is
-- persisted so a database read does not grant an active browser session.
CREATE TABLE IF NOT EXISTS auth_sessions (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_auth_sessions_expires_at ON auth_sessions(expires_at);

-- Curated, user-visible agent activity. This intentionally excludes model
-- prompts, hidden reasoning, raw tool input, and raw tool output.
CREATE TABLE IF NOT EXISTS activity_events (
    id TEXT PRIMARY KEY,
    conversation_id TEXT REFERENCES conversations(id) ON DELETE SET NULL,
    run_id TEXT,
    category TEXT NOT NULL,
    title TEXT NOT NULL,
    detail TEXT,
    tone TEXT NOT NULL DEFAULT 'normal',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_activity_events_created_at
    ON activity_events(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_activity_events_conversation
    ON activity_events(conversation_id, created_at DESC);

-- Durable user-facing goals and operational work tracking. Goal tasks describe
-- outcomes in the user's language; work runs/steps describe safe execution
-- progress without retaining prompts, reasoning, or raw tool payloads.
CREATE TABLE IF NOT EXISTS goals (
    id TEXT PRIMARY KEY,
    conversation_id TEXT REFERENCES conversations(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    objective TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    blocker TEXT,
    archived_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_goals_status_updated
    ON goals(status, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_goals_conversation
    ON goals(conversation_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS goal_tasks (
    id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    summary TEXT,
    blocker TEXT,
    source_type TEXT,
    source_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_goal_tasks_goal_position
    ON goal_tasks(goal_id, position);
CREATE INDEX IF NOT EXISTS idx_goal_tasks_source
    ON goal_tasks(source_type, source_id);

CREATE TABLE IF NOT EXISTS work_runs (
    id TEXT PRIMARY KEY,
    goal_id TEXT REFERENCES goals(id) ON DELETE SET NULL,
    task_id TEXT REFERENCES goal_tasks(id) ON DELETE SET NULL,
    conversation_id TEXT REFERENCES conversations(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    source_type TEXT,
    source_id TEXT,
    status TEXT NOT NULL DEFAULT 'queued',
    summary TEXT NOT NULL,
    current_phase TEXT,
    error TEXT,
    visibility TEXT NOT NULL DEFAULT 'significant',
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    started_at TEXT,
    finished_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_work_runs_status_updated
    ON work_runs(status, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_work_runs_goal_updated
    ON work_runs(goal_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_work_runs_conversation_updated
    ON work_runs(conversation_id, updated_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_work_runs_source
    ON work_runs(source_type, source_id)
    WHERE source_type IS NOT NULL AND source_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS work_run_steps (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES work_runs(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    summary TEXT,
    error TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_work_run_steps_run_sequence
    ON work_run_steps(run_id, sequence);

CREATE TABLE IF NOT EXISTS worker_status (
    worker_key TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    status TEXT NOT NULL,
    current_run_id TEXT REFERENCES work_runs(id) ON DELETE SET NULL,
    detail TEXT,
    last_started_at TEXT,
    last_success_at TEXT,
    last_error_at TEXT,
    last_error TEXT,
    updated_at TEXT NOT NULL
);

-- Consequential tool calls waiting for owner authorization. Tool arguments are
-- retained server-side so an approved action can execute exactly once.
CREATE TABLE IF NOT EXISTS pending_actions (
    id TEXT PRIMARY KEY,
    batch_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    call_id TEXT NOT NULL UNIQUE,
    tool_name TEXT NOT NULL,
    arguments TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    effect TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    result_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    resolved_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_pending_actions_conversation
    ON pending_actions(conversation_id, status, created_at);
CREATE INDEX IF NOT EXISTS idx_pending_actions_batch
    ON pending_actions(batch_id, status);

-- Conversation summaries for context compression
CREATE TABLE IF NOT EXISTS conversation_summaries (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    summary TEXT NOT NULL,
    messages_summarized INTEGER NOT NULL DEFAULT 0,
    last_message_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Files and Attachments
CREATE TABLE IF NOT EXISTS files (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    mime_type TEXT,
    size INTEGER NOT NULL,
    path TEXT NOT NULL,
    conversation_id TEXT REFERENCES conversations(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Link messages to files
CREATE TABLE IF NOT EXISTS message_attachments (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    PRIMARY KEY (message_id, file_id)
);

-- Asynchronous imports that turn user-provided chat exports into durable memory.
CREATE TABLE IF NOT EXISTS chat_imports (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL UNIQUE REFERENCES files(id) ON DELETE CASCADE,
    format TEXT NOT NULL DEFAULT 'auto',
    status TEXT NOT NULL DEFAULT 'queued',
    total_messages INTEGER NOT NULL DEFAULT 0,
    analyzed_messages INTEGER NOT NULL DEFAULT 0,
    memories_saved INTEGER NOT NULL DEFAULT 0,
    nodes_saved INTEGER NOT NULL DEFAULT 0,
    edges_saved INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_chat_imports_status_updated
    ON chat_imports(status, updated_at DESC);
