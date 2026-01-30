# Jossie2 — TODO

Items are grouped by area. Each item includes enough context to implement without reading the full codebase first. Check off items as they're completed.

---

## Tests

There are currently zero tests anywhere in the workspace.

- [ ] **jossie-core: IntegrationRegistry unit tests**
  File: `crates/jossie-core/src/integration.rs`
  - Test `register()` + `all_tool_definitions()` returns correct tools
  - Test `execute()` dispatches to the right integration
  - Test `execute()` with unknown tool name returns `is_error: true` result
  - Create a mock `Integration` impl for testing

- [ ] **jossie-core: Role round-trip tests**
  File: `crates/jossie-core/src/types.rs`
  - Test `Role::as_str()` and `FromStr` round-trip for all variants
  - Test serde serialization matches `rename_all = "lowercase"`

- [ ] **jossie-db: Database CRUD tests**
  File: `crates/jossie-db/src/lib.rs`
  - Use an in-memory SQLite (`sqlite::memory:`) for tests
  - Test `create_conversation` + `get_conversation` + `list_conversations`
  - Test `save_message` + `get_messages` ordering
  - Test `memory_save` + `memory_search` FTS matching
  - Test `memory_save` overwrites existing key

- [ ] **jossie-llm: Request building tests**
  File: `crates/jossie-llm/src/lib.rs`
  - `build_messages()` and `build_tools()` are private. Either make them `pub(crate)` or test via a mock HTTP server.
  - Test that empty tool list produces `tools: None` in the request
  - Test that `Role::Tool` messages always include `content` (even if empty)

- [ ] **jossie-server: HTTP endpoint tests**
  File: `crates/jossie-server/src/lib.rs`
  - Test auth middleware rejects missing/wrong Bearer token (returns 401)
  - Test auth middleware accepts correct token
  - Test `GET /api/conversations` returns empty list then populated list
  - Test `POST /api/chat` end-to-end with a mock LLM (requires refactoring `LlmClient` behind a trait or using a local HTTP mock server like `wiremock`)

- [ ] **jossie-integration-memory: Tool execution tests**
  File: `crates/jossie-integration-memory/src/lib.rs`
  - Test `execute("memory_save", ...)` then `execute("memory_search", ...)` returns saved content
  - Test unknown tool name returns error

---

## System Prompt

There is no system prompt. Conversations start with the user's first message only.

- [ ] **Add configurable system prompt**
  - Add `system_prompt: String` field to `LlmConfig` in `crates/jossie-core/src/config.rs`
  - Add a default value in `config.toml` (e.g. "You are Jossie, a helpful assistant.")
  - In `jossie-server/src/lib.rs`, in both `run_agent_loop()` and `handle_ws()`, prepend a `Role::System` message to the messages list before calling `llm.complete()`. Do NOT save it to the DB — inject it at call time.

---

## Agent Loop Hardening

- [ ] **Add iteration limit to agent loop**
  Location: `crates/jossie-server/src/lib.rs`, functions `run_agent_loop()` and `handle_ws()`
  Both contain `loop { ... }` with no bound. Add a configurable max (e.g. 10) and return an error or a "max iterations reached" message when exceeded.

- [ ] **Structured error responses from HTTP endpoints**
  Currently `chat_handler`, `list_conversations`, `get_messages` return bare `StatusCode::INTERNAL_SERVER_ERROR`. Replace with `Json<ErrorResponse>` containing `{"error": "..."}`. Use a custom `IntoResponse` impl or axum error handling pattern.

---

## Email Integration

Crate: `crates/jossie-integration-email/`
Current state: `Integration` trait is implemented but `execute()` returns "not yet implemented" for all tools.

- [ ] **Add `async-imap` and `lettre` dependencies**
  Add to `crates/jossie-integration-email/Cargo.toml`:
  ```toml
  async-imap = "0.10"
  async-native-tls = "0.5"
  lettre = { version = "0.11", features = ["tokio1-native-tls", "builder"] }
  ```

- [ ] **Implement IMAP connection management**
  In `crates/jossie-integration-email/src/lib.rs`:
  - Store full `EmailConfig` fields (host, port, username, password)
  - Create an async method to connect to IMAP, login, and return a session
  - Connections should be created per-call (simplest) or pooled (optimization)

- [ ] **Implement `email_search` tool**
  - Connect to IMAP, SELECT INBOX (or specified folder)
  - Run IMAP SEARCH command with the query
  - Return list of matching message summaries (subject, from, date, UID)
  - Limit results (e.g. 20)

- [ ] **Implement `email_read` tool**
  - Add tool definition with parameter `uid: string`
  - Fetch full message by UID, parse headers + body
  - Return formatted text (from, to, subject, date, body)

- [ ] **Implement `email_send` tool**
  - Use `lettre` to build and send message via SMTP
  - Parameters: `to`, `subject`, `body` (already defined in tool schema)
  - Return confirmation or error

- [ ] **Implement `email_list_folders` tool**
  - Add tool definition (no parameters)
  - List all IMAP mailboxes/folders
  - Return as JSON array

---

## Google Integration

Crate: `crates/jossie-integration-google/`
Current state: `Integration` trait is implemented but `execute()` returns "not yet implemented".

- [ ] **Add OAuth2 dependencies**
  Add to `crates/jossie-integration-google/Cargo.toml`:
  ```toml
  oauth2 = "5"
  ```
  `reqwest` is already a workspace dep.

- [ ] **Implement OAuth2 token management**
  - Add `refresh_token: String` and `token_url: String` fields to `GoogleConfig` in `crates/jossie-core/src/config.rs`
  - Implement token refresh flow: use `oauth2` crate to exchange refresh token for access token
  - Cache access token, refresh when expired
  - Store token state in the `GoogleIntegration` struct (behind a `tokio::sync::RwLock`)

- [ ] **Implement `gmail_search` tool**
  - Call Gmail API: `GET https://gmail.googleapis.com/gmail/v1/users/me/messages?q={query}`
  - Return list of message IDs + snippet

- [ ] **Implement `gmail_read` tool**
  - Add tool definition with parameter `message_id: string`
  - Call Gmail API: `GET https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full`
  - Parse and return headers + decoded body

- [ ] **Implement `gmail_send` tool**
  - Add tool definition with parameters `to`, `subject`, `body`
  - Build RFC 2822 message, base64url encode
  - Call Gmail API: `POST https://gmail.googleapis.com/gmail/v1/users/me/messages/send`

- [ ] **Implement `drive_search` tool**
  - Call Drive API: `GET https://www.googleapis.com/drive/v3/files?q={query}`
  - Return file names, IDs, mimeTypes

- [ ] **Implement `drive_read` tool**
  - Add tool definition with parameter `file_id: string`
  - Call Drive API to export/download file content
  - For Google Docs, export as plain text; for other files, return metadata

---

## Telegram Bot

Crate: `crates/jossie-telegram/`
Current state: Empty struct with `is_configured()` only.

- [ ] **Add `teloxide` dependency**
  Add to `crates/jossie-telegram/Cargo.toml`:
  ```toml
  teloxide = { version = "0.13", features = ["macros"] }
  ```
  Also add `jossie-server` or extract the agent loop into a shared location.

- [ ] **Implement bot message handler**
  - On incoming text message, map Telegram `chat_id` (i64) to a `conversation_id` (Uuid)
  - Maintain a persistent mapping (add a `telegram_chats` table to the DB schema, or use an in-memory HashMap)
  - Create conversation if first message from this chat_id
  - Run the agent loop (reuse `run_agent_loop` from jossie-server, or extract it into a shared crate)
  - Send the assistant's response back as a Telegram message

- [ ] **Wire bot startup into main.rs**
  - If `config.telegram.bot_token` is non-empty, spawn the bot as a background tokio task
  - The bot needs access to the same `Database`, `LlmClient`, and `IntegrationRegistry`

- [ ] **Handle long responses**
  Telegram messages have a 4096 character limit. Split long responses into multiple messages.

---

## WebSocket Streaming

Current state: The WebSocket handler in `jossie-server/src/lib.rs` (`handle_ws()`) uses non-streaming `llm.complete()`. It works but doesn't stream tokens.

- [ ] **Stream final assistant response via WebSocket**
  In `handle_ws()`, when the agent loop reaches the final response (no tool calls):
  - Use `llm.complete_stream()` instead of `complete()`
  - Spawn a task that reads from the `mpsc::Receiver<StreamEvent>`
  - For each `StreamEvent::Delta(text)`, send a WebSocket frame: `{"type": "delta", "content": "..."}`
  - On `StreamEvent::Done`, send `{"type": "done"}` and save the accumulated content to DB
  - On `StreamEvent::ToolCalls(...)`, continue the agent loop as before

---

## Web UI

There is no frontend. The server only exposes API endpoints.

- [ ] **Create minimal chat UI**
  - Add a static file serving route in `jossie-server` (e.g. `GET /` serves `static/index.html`)
  - Build a single-page HTML/JS chat interface:
    - Text input + send button
    - Message list showing user and assistant messages
    - Connect via WebSocket to `/api/chat/stream`
    - Display streaming deltas as they arrive
    - Show tool call activity (optional)
  - Store static files in `crates/jossie-server/static/` and include via `include_str!` or `tower-http::services::ServeDir`

---

## Configuration & Deployment

- [ ] **Support env var overrides for secrets**
  `config.toml` contains `api_key`, `auth_token`, `password` etc. in plain text. Support environment variable overrides (e.g. `JOSSIE_LLM_API_KEY`) so secrets don't need to be in the file. Either use a crate like `config` or manually check env vars in `main.rs` after loading the TOML.

- [ ] **Add `.env` file support**
  Add `dotenvy` to workspace deps. Call `dotenvy::dotenv().ok()` at the top of `main()`.

- [ ] **Dockerfile**
  Multi-stage build: compile with `rust:latest`, run with `debian:bookworm-slim`. Copy binary + config.toml.

---

## Code Quality

- [ ] **Suppress or fix the one compiler warning**
  In `crates/jossie-llm/src/lib.rs:219`, the assignment `accumulated_tool_calls = Vec::new()` after sending tool calls is dead code because the function returns on the next line. Remove the reassignment.

- [ ] **Use `JossieError` consistently**
  `crates/jossie-core/src/error.rs` defines `JossieError` but nothing uses it — everything uses `anyhow::Result`. Decide whether to adopt `JossieError` at crate boundaries (recommended for `jossie-server` HTTP responses) or remove it.

- [ ] **Extract agent loop into shared code**
  `run_agent_loop()` lives in `jossie-server` but `jossie-telegram` will need the same logic. Extract it into `jossie-core` or a new `jossie-agent` crate that depends on core, llm, and db.

- [ ] **Deduplicate migration SQL**
  The schema exists in two places: `crates/jossie-db/migrations.sql` (used at runtime) and `migrations/001_init.sql` (unused). Delete the `migrations/` directory or switch to sqlx's migration system.
