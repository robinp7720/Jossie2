# Jossie Web UI

This frontend is a standalone Vite + React app for the Jossie HTTP and WebSocket API.

## Quick start

```sh
cd frontend
npm install
npm run dev
```

Then open the URL printed by Vite (default http://localhost:5173).

## Configuration

In the UI, set:
- Base URL: the Jossie server address (example: http://localhost:8080)
- Token: the auth token from `config.toml`

Streaming uses the WebSocket endpoint at `/api/chat/stream`.
