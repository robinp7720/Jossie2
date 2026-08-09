# Jossie2 — Agent Development Guide

## What This Is

Jossie2 is an agentic LLM companion built as a Rust workspace plus a React frontend. It exposes an authenticated HTTP/WebSocket API, serves a browser UI, supports a Telegram bot frontend, persists long-term state in SQLite, and can run background polling/scheduled work.

This file reflects the current codebase, not the older roadmap docs. If another doc disagrees with source, trust the source.

## Project Layout

```text
Jossie2/
  Cargo.toml                    # workspace root + binary package
  config.sample.toml            # committed sample config; copy to config.toml locally
  src/main.rs                   # loads config, wires integrations, starts server/bot
  src/event_loop.rs             # background-worker facade
  src/event_loop/               # event batching, scheduling, heartbeat, worker logic
  frontend/                     # Vite + React web UI
  crates/
    jossie-core/               # shared types, config, integration trait/registry
    jossie-llm/                # OpenAI-compatible Responses API client
    jossie-db/                 # SQLite persistence + embedded SQL migrations
    jossie-server/             # axum API, auth, handlers, agent loop
    jossie-telegram/           # teloxide Telegram frontend
    jossie-integration-memory/ # long-term memory tools
    jossie-integration-graph/  # knowledge graph tools
    jossie-integration-email/  # IMAP/SMTP email tools
    jossie-integration-mail/   # provider-neutral mail tools
    jossie-integration-google/ # Gmail, Drive, Calendar, OAuth, polling
    jossie-integration-browser/# headless browser + DuckDuckGo search
    jossie-integration-http/   # outbound HTTP requests with guardrails
    jossie-integration-scheduler/ # scheduled tasks + out-of-band messages
    jossie-integration-files/  # uploaded files and chat-export imports
```

## Workspace Summary

| Component | Current role |
|---|---|
| `jossie-core` | Shared config, message/tool types, integration registry, onboarding types |
| `jossie-llm` | Non-streaming and streaming Responses API client |
| `jossie-db` | Conversations, messages, memory, graph, accounts, events, scheduler, OOB queue |
| `jossie-server` | Authenticated REST + WS API, static frontend hosting, agent loop |
| `jossie-telegram` | Private Telegram frontend with media, voice transcription, approvals, commands, and sustained typing status |
| `integration-memory` | `memory_save`, `memory_search`, `memory_list_keys`, `memory_list_all` |
| `integration-graph` | graph node/edge mutation and query tools |
| `integration-email` | Internal IMAP/SMTP provider, onboarding, and polling |
| `integration-mail` | Agent-visible provider-neutral mail search/read/send tools |
| `integration-google` | Google OAuth onboarding, Gmail provider operations, Drive, Calendar, polling |
| `integration-browser` | page fetch/render via headless Chrome, DDG-style search |
| `integration-http` | generic HTTP requests with SSRF-style blocking and domain controls |
| `integration-scheduler` | one-shot/recurring tasks, cancel/list, out-of-band notifications |
| `frontend` | React chat UI, onboarding/accounts UI, knowledge graph view |

## Core Architecture

### Integration System

Every integration implements `jossie_core::integration::Integration` through `crates/jossie-core/src/integration.rs`:

```rust
#[async_trait]
pub trait Integration: Send + Sync {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<ToolDefinition>;
    fn agent_tools(&self) -> Vec<ToolDefinition> { ... }
    fn show_in_onboarding(&self) -> bool { ... }
    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String>;
    async fn check_onboarding(&self) -> anyhow::Result<OnboardingStatus> { ... }
    async fn poll(&self) -> anyhow::Result<()> { ... }
}
```

`IntegrationRegistry` stores integrations, maps tool names to implementations, retries transient-looking failures, truncates oversized tool output, and appends quality hints for empty/partial/error-like results.

### Agent Loop

The public agent facade lives in `crates/jossie-server/src/agent.rs`; goal tracking, prompts, tools, run modes, event mode, and knowledge handling are separated under `src/agent/`.

Current behavior:

1. User message is saved to SQLite.
2. Recent conversation history is loaded with a configurable context cap.
3. A dynamic system prompt is prepended.
   It includes the configured base prompt, current time, memory index, selected memory entries, and graph context.
4. The LLM is called through `LlmClient`.
5. Tool calls are executed through the registry and saved as assistant/tool messages.
6. The loop repeats until a final assistant reply is produced or `max_agent_iterations` is reached.

Notable current features:

- There is a hard iteration limit: `llm.max_agent_iterations` defaults to `20`.
- WebSocket chat uses real token streaming via `complete_stream()`.
- Optional self-reflection is supported via `kg_llm` when `llm.enable_self_reflection = true`.
- Scheduler tools get `__conversation_id` injected before execution.
- The server tracks active conversations to avoid concurrent processing of the same conversation.

### Background Event Loop

`src/event_loop.rs` is the facade for the background worker, whose domain files live under `src/event_loop/`. It:

- polls integrations that implement `poll()`
- processes queued `integration_events`
- executes due scheduled tasks
- delivers queued out-of-band messages

The event loop is started unconditionally from `src/main.rs`, so polling, scheduled tasks, heartbeat checks, and web-visible background activity continue without Telegram. Telegram delivery is used when a linked chat is available.

## Database Reality

SQLite schema is embedded in `crates/jossie-db/migrations.sql` and applied by `Database::migrate()` in `crates/jossie-db/src/lib.rs`. Domain-specific operations live under `src/repositories/` while `Database` remains the facade.

Current schema includes:

- `conversations`
- `messages`
- `memory` (FTS5)
- `memory_metadata`
- `telegram_chats`
- `integration_settings`
- `integration_accounts`
- `integration_events`
- `graph_nodes`
- `graph_edges`
- `scheduled_tasks`
- `out_of_band_messages`
- `conversation_summaries`
- `auth_sessions`, `activity_events`, `pending_actions`
- `goals`, `goal_tasks`, `work_runs`, `work_run_steps`, `work_run_checkpoints`
- `worker_status`, `telegram_goal_notifications`
- `files`, `message_attachments`, `chat_imports`

Do not assume all IDs or timestamps follow one format. Conversations/messages/tasks/events are generally app-generated UUID strings, but some keys are arbitrary strings and Telegram chat IDs are integers. Some timestamps are written as RFC3339 by application code, while schema defaults still use SQLite `datetime('now')`.

## HTTP API And Frontends

The router is assembled in `crates/jossie-server/src/lib.rs`.

Authentication:

- Protected routes accept `Authorization: Bearer <token>`.
- The auth middleware also accepts `?token=...`, mainly for WebSockets/browser usage.
- `/api/auth/login`, `/api/auth/session`, `/api/health`, and `/oauth/callback` are public.
- `/setup/google` is auth-protected.
- Static frontend files from `frontend/dist` are served as the fallback service.

Current API surface:

| Route | Method | Notes |
|---|---|---|
| `/api/chat` | `POST` | non-streaming chat |
| `/api/chat/stream` | `GET` | WebSocket streaming chat |
| `/api/events` | `GET` | authenticated workspace-event WebSocket |
| `/api/conversations` | `GET`, `POST` | search/page conversations or create a thread |
| `/api/conversations/{id}` | `PATCH`, `DELETE` | rename/archive or permanently delete an archived thread |
| `/api/conversations/{id}/messages` | `GET` | optional `limit`, `before`, or `around` windowing |
| `/api/conversations/{id}/export` | `GET` | visible transcript as Markdown or JSON |
| `/api/files`, `/api/files/{id}` | `POST`, `GET`, `DELETE` | upload, download, or remove an unused file |
| `/api/chat-imports` | `POST` | start a chat-export import |
| `/api/graph` | `GET` | returns graph nodes/edges, optional `?limit=` |
| `/api/dashboard`, `/api/memories`, `/api/activity` | `GET` | browser workspace data |
| `/api/work`, `/api/goals/{id}`, `/api/work/runs/{id}` | `GET` | durable work, goal, and run state |
| `/api/actions/pending` | `GET` | pending external-action approvals |
| `/api/onboarding` | `GET` | integration onboarding status |
| `/api/config/accounts` | `GET`, `POST` | list/add integration accounts |
| `/api/config/accounts/{id}` | `PATCH`, `DELETE` | update/delete integration account |
| `/setup/google` | `GET` | start Google OAuth |
| `/oauth/callback` | `GET` | complete Google OAuth |
| `/api/health` | `GET` | DB health |

Errors are already returned as structured JSON from `crates/jossie-server/src/errors.rs`.

## Config Reality

The committed config file is `config.sample.toml`. The binary reads `config.toml` from the current working directory.

Current top-level config sections are defined in `crates/jossie-core/src/config.rs`:

- `[server]`
- `[llm]`
- `[database]`
- `[telegram]`
- `[email]`
- `[google]`
- `[http]`
- `[heartbeat]`

Environment overrides are implemented in `src/main.rs`:

- `JOSSIE_SERVER_AUTH_TOKEN`
- `JOSSIE_SERVER_PUBLIC_BASE_URL`
- `JOSSIE_LLM_API_KEY`
- `JOSSIE_LLM_SYSTEM_PROMPT`
- `JOSSIE_LLM_MAX_CONTEXT_MESSAGES`
- `JOSSIE_LLM_EVENT_MAX_CONTEXT_MESSAGES`
- `JOSSIE_TELEGRAM_BOT_TOKEN`
- `JOSSIE_TELEGRAM_MAX_DOWNLOAD_BYTES`
- `JOSSIE_TELEGRAM_FFMPEG_PATH`
- `JOSSIE_LLM_TRANSCRIPTION_MODEL`
- `JOSSIE_LLM_MAX_ATTACHMENT_BYTES_PER_REQUEST`
- `JOSSIE_EMAIL_USERNAME`
- `JOSSIE_EMAIL_PASSWORD`
- `JOSSIE_EMAIL_IMAP_HOST`
- `JOSSIE_EMAIL_SMTP_HOST`
- `JOSSIE_GOOGLE_CLIENT_ID`
- `JOSSIE_GOOGLE_CLIENT_SECRET`
- `JOSSIE_GOOGLE_REFRESH_TOKEN`

Other runtime knobs worth knowing:

- `JOSSIE_LOG_JSON=1` switches tracing output to JSON.
- `llm.kg_model` can use a cheaper second model for graph extraction/self-reflection.
- `server.cors_origins` and `server.max_request_body_bytes` are active.

## Integration Status

| Integration | Status | Notes |
|---|---|---|
| Memory | Implemented | FTS-backed long-term memory |
| Graph | Implemented | node/edge storage + exploration/search tools |
| Email | Implemented | generic IMAP/SMTP accounts; onboarding checks for configured/default accounts |
| Google | Implemented | OAuth flow, account storage, Gmail/Drive/Calendar tools, polling for Gmail/calendar events |
| Browser | Implemented | headless page reading and search; useful for JS-heavy pages |
| HTTP | Implemented | outbound requests with host/IP validation and optional allow-list |
| Scheduler | Implemented | scheduled agent runs + queued user notifications |
| Telegram | Implemented | private-chat bot, chat linking, typing status, commands, media/albums, voice transcription, approvals, proactive delivery |

## Testing And Build

The workspace is not test-free anymore.

At the time this guide was updated:

- `cargo test --workspace -q` passes
- `cargo check -q` passes

There are Rust tests across the core, database, LLM, server, integrations, Telegram, and background-worker code, plus Vitest component and utility tests in the frontend.

Useful commands:

```sh
cargo check
cargo test --workspace
cargo run
cd frontend && npm ci && npm run build
```

If you want the served web UI to work, build `frontend/dist` first.

## Conventions

- Rust edition `2024`, resolver `3`
- Shared dependencies live in `[workspace.dependencies]`
- `tracing` is used for logs
- `anyhow::Result` is common across integration and service boundaries
- `serde` derives are used for API-facing and stored types
- `async-trait` is used for integration traits

## Working Rules For Agents

- Explore source before trusting older docs. `README.md`, `TODO.md`, and `WEB_API.md` still contain stale claims.
- Prefer updating the real behavior or updating the docs, but do not preserve known-false descriptions.
- After making changes, commit them with `git commit`.
- If a task affects the browser UI, remember the Rust server serves built assets from `frontend/dist`, not raw Vite sources.
