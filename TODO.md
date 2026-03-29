# Jossie2 — TODO

Current backlog only. Completed historical work has been removed so this file reflects the repository as it exists now.

## High Priority

- [ ] Add HTTP endpoint integration tests in `crates/jossie-server`
  Cover auth middleware, `POST /api/chat`, `GET /api/conversations`, `GET /api/conversations/{id}/messages`, and the onboarding/accounts endpoints.

- [ ] Add an agent-loop integration test with a mock LLM
  The loop now includes tool execution, context-window trimming, self-reflection, and streaming paths. It needs explicit end-to-end coverage.

- [ ] Decouple the background event loop from Telegram startup
  `src/main.rs` currently starts `src/event_loop.rs` only when Telegram is configured, even though scheduled tasks and integration polling are not inherently Telegram-specific.

- [ ] Define delivery behavior for proactive notifications when Telegram is not configured
  Right now integration events and OOB notifications are routed through Telegram-linked conversations. Decide whether web-only users should get in-app delivery, persisted inbox-style notifications, or some other channel.

## Testing

- [ ] Expand `jossie-db` tests to cover integration accounts, integration events, scheduled tasks, and graph queries

- [ ] Add tests for Google OAuth callback and public base URL normalization in realistic handler-level flows

- [ ] Add frontend smoke checks or a lightweight build/test step so the served `frontend/dist` contract is covered in CI

## Docs

- [ ] Keep `README.md`, `WEB_API.md`, and `AGENTS.md` aligned when integrations, routes, or startup behavior change

- [ ] Document the frontend build requirement more prominently
  The Rust server serves `frontend/dist`, so a fresh checkout without a frontend build will not have a working UI.

## Product / Architecture

- [ ] Decide whether conversation summaries are active or dead code
  The database schema includes `conversation_summaries`, but it is not obvious from the current top-level flow whether summarization is fully wired into normal operation.

- [ ] Review browser-search strategy
  `jossie-integration-browser` relies on web scraping/search behavior that may be brittle against bot blocking. Decide whether to keep this path, harden it, or replace it.

- [ ] Review HTTP integration policy defaults
  Confirm whether the current combination of `allowed_domains` and SSRF-style IP/host validation matches the intended production threat model.
