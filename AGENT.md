# Jossie2 — Agent Development Guide

## What This Is

An agentic LLM assistant with a plugin-based integration system, WebSocket streaming API, and multiple chat frontends. Rust workspace, edition 2024.

## Project Layout

```
Jossie2/
  Cargo.toml              # workspace root + binary package
  config.toml             # runtime config (not committed with real secrets)
  src/main.rs             # binary: loads config, inits DB, registers integrations, starts server
  migrations/001_init.sql # reference copy of schema (not used at runtime)
  crates/
    jossie-core/          # traits, types, config, registry — no IO, no side effects
    jossie-llm/           # OpenAI-compatible LLM client (streaming + non-streaming)
    jossie-db/            # SQLite persistence via sqlx; embeds its own migrations.sql
    jossie-server/        # axum HTTP + WebSocket API, agent loop, auth middleware
    jossie-telegram/      # Telegram bot frontend (STUB)
    jossie-integration-email/   # IMAP + SMTP (STUB)
    jossie-integration-google/  # Gmail, Drive (STUB)
    jossie-integration-memory/  # keyword/FTS5 memory (COMPLETE)
```

## Dependency Graph

```
jossie-core  (no internal deps — everything depends on this)
    ^
    |--- jossie-llm          (core)
    |--- jossie-db           (core)
    |--- jossie-server       (core, llm, db)
    |--- jossie-integration-memory  (core, db)
    |--- jossie-integration-email   (core)
    |--- jossie-integration-google  (core)
    |--- jossie-telegram     (core, llm, db)
```

The binary (`src/main.rs`) depends on core, llm, db, server, and integration-memory.

## Architecture

### Integration System

Every integration implements the `Integration` trait (`jossie-core/src/integration.rs`):

```rust
#[async_trait]
pub trait Integration: Send + Sync {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<ToolDefinition>;              // OpenAI function-calling schema
    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String>;
}
```

`IntegrationRegistry` collects integrations and dispatches `ToolCall`s by tool name. To add a new integration:

1. Create a crate under `crates/jossie-integration-<name>/`
2. Implement `Integration` for your struct
3. Register it in `main.rs`: `registry.register(Arc::new(YourIntegration::new(...)))`

### Agent Loop (`jossie-server/src/lib.rs`)

1. User sends message -> saved to DB
2. Load conversation history + all tool definitions from registry
3. Call `LlmClient::complete()` (non-streaming) or `complete_stream()` (streaming)
4. If LLM returns `tool_calls` -> execute each via registry -> append tool results as `Role::Tool` messages -> loop back to step 3
5. If LLM returns plain text -> save as `Role::Assistant` message, return to client

The agent loop has no hardcoded iteration limit. Add one if needed.

### Database (`jossie-db`)

SQLite via sqlx. Schema is embedded in `crates/jossie-db/migrations.sql` and applied via `Database::migrate()` (raw SQL execution, not sqlx migrations).

Tables: `conversations`, `messages`, `memory` (FTS5 virtual table), `memory_metadata`.

All IDs are UUID v4 stored as TEXT. All timestamps are RFC3339 TEXT.

### LLM Client (`jossie-llm`)

OpenAI-compatible. Two modes:
- `complete()` — non-streaming, returns `(String, Vec<ToolCall>)`
- `complete_stream()` — SSE streaming via `mpsc::Sender<StreamEvent>`, accumulates tool call deltas

The streaming parser handles SSE `data:` lines and `[DONE]` sentinel.

### HTTP API (`jossie-server`)

All routes require `Authorization: Bearer <token>` header (configured in `config.toml`).

| Route | Method | Description |
|---|---|---|
| `/api/chat` | POST | Sync chat. Body: `{"message": "...", "conversation_id": "..."}` |
| `/api/chat/stream` | GET | WebSocket upgrade. Send JSON frames, receive streaming responses |
| `/api/conversations` | GET | List all conversations |
| `/api/conversations/{id}/messages` | GET | Get messages for a conversation |

WebSocket messages use the same `ChatRequest` schema. Responses are JSON with `{"type": "message", ...}` or `{"type": "tool_result", ...}`.

## What's Implemented vs Stubbed

| Crate | Status | Notes |
|---|---|---|
| jossie-core | Complete | Types, traits, config, registry |
| jossie-llm | Complete | Streaming + non-streaming |
| jossie-db | Complete | CRUD + FTS5 memory |
| jossie-server | Complete | HTTP, WS, auth, agent loop |
| jossie-integration-memory | Complete | `memory_save`, `memory_search` |
| jossie-integration-email | **Stub** | Tool defs exist, `execute()` returns "not yet implemented" |
| jossie-integration-google | **Stub** | Tool defs exist, `execute()` returns "not yet implemented" |
| jossie-telegram | **Stub** | Just a struct with `is_configured()` |

## What to Work On Next

Roughly in priority order:

### 1. Tests
There are zero tests. Start with:
- Unit tests for `IntegrationRegistry` (register, dispatch, unknown tool)
- Unit tests for `Database` (CRUD operations, memory FTS)
- Integration test for the agent loop (mock LLM responses)

### 2. Email Integration (`jossie-integration-email`)
Implement the `execute()` method. Recommended crates:
- `async-imap` for IMAP (search, read)
- `lettre` for SMTP (send)
- Add tools: `email_search`, `email_read`, `email_send`, `email_list_folders`

### 3. Google Integration (`jossie-integration-google`)
Implement OAuth2 token flow and API calls. Recommended crates:
- `oauth2` for token management
- `reqwest` for API calls (already a workspace dep)
- Add tools: `gmail_search`, `gmail_read`, `gmail_send`, `drive_search`, `drive_read`

### 4. Telegram Bot (`jossie-telegram`)
Wire up `teloxide` as a chat frontend:
- Map Telegram `chat_id` to a `conversation_id`
- Reuse the agent loop from `jossie-server`
- Register and start the bot in `main.rs`

### 5. Streaming in WebSocket Handler
The WS handler currently uses non-streaming `complete()` in the agent loop. For the final assistant response, switch to `complete_stream()` and pipe `StreamEvent::Delta` frames to the WebSocket for real-time token streaming.

### 6. System Prompt
There's no system prompt injected into conversations. Add a configurable system prompt (in `config.toml` or per-conversation) prepended as a `Role::System` message.

### 7. Error Handling Improvements
- Add an iteration limit to the agent loop to prevent infinite tool-calling loops
- Return structured error JSON from HTTP endpoints instead of bare 500s
- Use `JossieError` more consistently (it exists but isn't used much)

### 8. Web UI
Serve a static web UI from axum. Consider a simple HTML/JS chat interface that connects via WebSocket.

## Build & Run

```sh
cargo build                    # compile workspace
cargo check                    # type-check only (faster)
cargo test                     # run tests (none exist yet)
cargo run                      # start server (needs config.toml with valid settings)
```

The server reads `config.toml` from the current working directory. Set `RUST_LOG=debug` for verbose logging.

## Conventions

- Edition 2024, resolver 3
- All shared deps are declared in `[workspace.dependencies]` and referenced with `{ workspace = true }`
- `async-trait` for async trait methods
- `anyhow::Result` for fallible functions, `thiserror` for typed errors
- UUIDs as `uuid::Uuid`, timestamps as `chrono::DateTime<Utc>`
- `tracing` for structured logging (not `log`)
- `serde` derive for all API-facing types
