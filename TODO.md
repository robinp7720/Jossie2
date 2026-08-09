# Jossie2 — TODO

Current backlog only. Completed historical work has been removed so this file reflects the repository as it exists now.

## High Priority

- [ ] Add an agent-loop integration test with a mock LLM
  The loop now includes tool execution, context-window trimming, self-reflection, and streaming paths. It needs explicit end-to-end coverage.

- [ ] Define delivery behavior for proactive notifications when Telegram is not configured
  Background processing now runs without Telegram and records web-visible activity, but dedicated inbox-style delivery for web-only users is still undecided.

## Testing

- [ ] Add tests for Google OAuth callback and public base URL normalization in realistic handler-level flows

- [ ] Add frontend smoke checks or a lightweight build/test step so the served `frontend/dist` contract is covered in CI

## Docs

- [ ] Keep `README.md`, `WEB_API.md`, and `AGENTS.md` aligned when integrations, routes, or startup behavior change

- [ ] Document the frontend build requirement more prominently
  The Rust server serves `frontend/dist`, so a fresh checkout without a frontend build will not have a working UI.

## Product / Architecture

- [ ] Review browser-search strategy
  `jossie-integration-browser` relies on web scraping/search behavior that may be brittle against bot blocking. Decide whether to keep this path, harden it, or replace it.

- [ ] Review HTTP integration policy defaults
  Confirm whether the current combination of `allowed_domains` and SSRF-style IP/host validation matches the intended production threat model.
