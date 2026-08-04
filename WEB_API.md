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

List all conversations.

Response:

```json
[
  {
    "id": "uuid-string",
    "title": "Telegram chat 12345",
    "created_at": "ISO-8601-timestamp",
    "updated_at": "ISO-8601-timestamp"
  }
]
```

#### `GET /api/conversations/{id}/messages`

Fetch conversation messages in chronological order.

Optional query parameters:

- `limit`: maximum number of most recent messages to return

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

Response:

```json
"generated-account-id"
```

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

Authentication is typically passed as `?token=...`.

Client message:

```json
{
  "message": "Hello",
  "conversation_id": "optional-uuid"
}
```

Server events:

#### Delta

```json
{
  "type": "delta",
  "content": "Hel"
}
```

#### Tool result

```json
{
  "type": "tool_result",
  "tool": "gmail_search",
  "result": "Search results JSON..."
}
```

#### Done

```json
{
  "type": "done",
  "conversation_id": "uuid"
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
