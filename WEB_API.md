# Jossie Web API Documentation

This document describes the current HTTP and WebSocket API exposed by the Jossie server.

## Authentication

Protected routes accept either:

1. `Authorization: Bearer <token>`
2. `?token=<token>` query parameter

The token comes from `config.toml` or `JOSSIE_SERVER_AUTH_TOKEN`.

Public routes:

- `GET /api/health`
- `GET /oauth/callback`

Everything else described here is auth-protected.

## REST API

### Chat

#### `POST /api/chat`

Send a message and wait for the final assistant response.

Request:

```json
{
  "message": "Hello Jossie!",
  "conversation_id": "optional-uuid"
}
```

Response:

```json
{
  "conversation_id": "uuid-string",
  "message": "Hello! How are you?"
}
```

If `conversation_id` is omitted, the server creates a new conversation.

### Files and Chat Imports

#### `POST /api/files`

Upload a file as multipart form data using the `file` field. The response contains `file_id` and `name`.

#### `GET` / `DELETE /api/files/{id}`

Download an uploaded attachment, or delete it while it is still an unused draft. Attached/imported files cannot be deleted through the draft cleanup endpoint.

#### `POST /api/chat-imports`

Queue an uploaded TXT or JSON chat export for background learning.

```json
{
  "file_id": "uploaded-file-uuid",
  "format": "auto"
}
```

`format` may be `auto`, `whatsapp`, `signal`, `chatgpt`, or `generic`. Auto-detection is recommended.

The response is a durable import record whose `status` is `queued`, `processing`, `completed`, or `failed`. Completed records include the total and analyzed message counts plus the number of memories, graph nodes, and graph edges saved.

#### `GET /api/chat-imports/{id}`

Fetch current import status. Large histories are analyzed in bounded chunks; when the export exceeds the analysis budget, chunks are sampled across the timeline with early and recent context retained.

### Conversations

#### `GET /api/conversations`

List and search conversations. Query parameters are `view=active|archived|all` (default `active`), `q`, `limit` (maximum `100`), and the stable `before=<conversation-id>` cursor. Results also include a visible-message preview, message count, and `matched_message_id` when search matched transcript content.

Response:

```json
[
  {
    "id": "uuid-string",
    "title": "Telegram chat 12345",
    "archived_at": null,
    "preview": "Most recent visible message",
    "matched_message_id": null,
    "message_count": 8,
    "created_at": "ISO-8601-timestamp",
    "updated_at": "ISO-8601-timestamp"
  }
]
```

#### `POST /api/conversations`

Create an empty conversation. The web UI uses this before the first reconnect-safe turn.

#### `PATCH` / `DELETE /api/conversations/{id}`

Patch `title` and/or `archived`. Permanent deletion requires an archived conversation with no active work, approval, open goal, or schedule. It removes conversation-specific history and exclusive attachments, but not global memory or graph knowledge.

#### `GET /api/conversations/{id}/export`

Download the visible user/assistant transcript with attachment metadata. `format` is `markdown` (default) or `json`; internal system/tool records and attachment binaries are excluded.

#### `GET /api/conversations/{id}/messages`

Fetch conversation messages in chronological order.

Optional query parameters:

- `limit`: maximum number of most recent messages to return
- `before`: return messages before a message UUID
- `around`: return a chronological window centered on a message UUID

`before` and `around` are mutually exclusive; `limit` is clamped to `1..=200`.

Response:

```json
[
  {
    "id": "uuid",
    "conversation_id": "uuid",
    "role": "user",
    "content": "Hi",
    "tool_calls": null,
    "tool_call_id": null,
    "name": null,
    "created_at": "..."
  }
]
```

### Knowledge Graph

#### `GET /api/graph`

Return graph nodes and edges for the frontend graph view.

Optional query parameters:

- `limit`: defaults to `500`

Response:

```json
{
  "nodes": [],
  "edges": []
}
```

### Integration Onboarding

#### `GET /api/onboarding`

Return onboarding status for each registered integration.

Response shape:

```json
[
  {
    "name": "google",
    "status": "Configured"
  },
  {
    "name": "email",
    "status": "RequiresAction",
    "details": {
      "fields": []
    }
  }
]
```

### Integration Accounts

#### `GET /api/config/accounts`

List stored integration accounts. Secrets are redacted in the response.

#### `POST /api/config/accounts`

Add an integration account.

Request:

```json
{
  "integration": "email",
  "name": "Work Email",
  "config": {
    "username": "me@example.com",
    "password": "secret_password",
    "imap_host": "imap.example.com",
    "imap_port": 993,
    "smtp_host": "smtp.example.com",
    "smtp_port": 587
  }
}
```

Current supported `integration` values:

- `email`
- `google`
- `todoist`
- `home_assistant`
- `notion`
- `spotify`

Response:

```json
"generated-account-id"
```

`GET /api/config/integration-types` returns the provider-declared connection
fields and whether OAuth setup is available. OAuth providers start at
`GET /setup/{provider}` and return to `/oauth/callback`.

`POST /api/integrations/webhooks/{provider}` is the unauthenticated provider
callback surface. Each provider must cryptographically verify its own request;
Todoist validates `X-Todoist-Hmac-SHA256` and deduplicates deliveries.

#### `DELETE /api/config/accounts/{id}`

Delete an integration account.

Response:

```json
null
```

### Google OAuth Setup

#### `GET /setup/google`

Start the Google OAuth flow. This route is auth-protected.

Optional query parameters:

- `account_name`

The server redirects to Google and later receives the callback on `/oauth/callback`.

#### `GET /oauth/callback`

Public OAuth callback endpoint. On success it stores a Google integration account in the database and returns a small HTML success/error page.

### Goals And Work Progress

#### `GET /api/work`

Return open goals, active and significant recent runs, worker health, upcoming schedules, and recent chat imports. Optional query parameters include `conversation_id`, `before`, `limit`, `include_quiet`, and `include_archived`.

#### `GET /api/goals/{id}` / `PATCH /api/goals/{id}`

Read a goal with its outcome tasks and run history, or rename/archive it.

#### `POST /api/goals/{id}/pause|resume|cancel`

Control future and currently executing work for a goal. Cancellation does not undo effects that already completed.

#### `GET /api/work/runs` / `GET /api/work/runs/{id}` / `POST /api/work/runs/{id}/cancel`

List significant run history with cursor, kind, and status filters; read the safe execution timeline for one run; or request that an active run stop at its next safe boundary.

### Health

#### `GET /api/health`

Public health endpoint.

Response:

```json
{
  "status": "ok",
  "db": "connected"
}
```

## WebSocket API

### `GET /api/chat/stream`

WebSocket endpoint for streaming chat responses.

The authenticated `/api/events` WebSocket also publishes `goal_updated`, `work_run_updated`, `work_step_updated`, and degraded `worker_status_updated` events. These contain user-visible summaries rather than hidden reasoning or raw tool payloads.

Authentication is typically passed as `?token=...`.

Client message:

```json
{
  "message": "Hello",
  "conversation_id": "optional-uuid",
  "client_message_id": "optional-idempotency-uuid",
  "file_ids": ["optional-upload-uuid"]
}
```

The browser supplies `client_message_id`. Retrying the same ID and content acknowledges the existing turn rather than creating a duplicate.

Server events include:

#### Accepted

```json
{
  "type": "message_accepted",
  "conversation_id": "uuid",
  "message_id": "uuid",
  "duplicate": false,
  "run_id": "uuid"
}
```

#### Assistant delta

```json
{
  "type": "assistant_delta",
  "conversation_id": "uuid",
  "run_id": "uuid",
  "content": "Hel"
}
```

Run lifecycle events include `run_started`, `assistant_thinking`, `tool_started`, `tool_finished`, `run_waiting_for_approval`, `run_completed`, `run_paused`, `run_cancelled`, and `error`. The same privacy-safe events are broadcast on `/api/events`, allowing clients to reconcile after reconnecting.

#### Completed

```json
{
  "type": "run_completed",
  "conversation_id": "uuid",
  "run_id": "uuid"
}
```

#### Error

```json
{
  "type": "error",
  "error": "Something went wrong"
}
```

## Error Responses

Protected REST endpoints return structured JSON errors:

```json
{
  "error": "message text"
}
```

Unauthorized requests return HTTP `401` with:

```json
{
  "error": "unauthorized"
}
```
