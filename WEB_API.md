# Jossie Web API Documentation

This document describes the HTTP and WebSocket API for the Jossie server.

## Authentication

All API endpoints (except root `/`, `/oauth/callback`) require authentication.

**Method:** Bearer Token or Query Parameter.

1.  **Header:** `Authorization: Bearer <YOUR_AUTH_TOKEN>`
2.  **Query Param:** `?token=<YOUR_AUTH_TOKEN>` (useful for WebSockets or simple browser tests)

The token is configured in `config.toml` (or env `JOSSIE_SERVER_AUTH_TOKEN`).

---

## REST API Endpoints

### 1. Chat

#### Send a Message
*   **POST** `/api/chat`
*   **Description:** Sends a message to the agent and awaits a complete response (blocking).
*   **Request Body:**
    ```json
    {
      "message": "Hello Jossie!",
      "conversation_id": "optional-uuid-string"
    }
    ```
    *   If `conversation_id` is omitted, a new conversation is created.
*   **Response:**
    ```json
    {
      "conversation_id": "uuid-string",
      "message": "Hello! How are you?"
    }
    ```

### 2. Conversations

#### List Conversations
*   **GET** `/api/conversations`
*   **Response:** Array of conversation objects.
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

#### Get Messages
*   **GET** `/api/conversations/{id}/messages`
*   **Description:** Retrieve full message history for a conversation.
*   **Response:** Array of message objects.
    ```json
    [
      {
        "id": "uuid",
        "role": "user",
        "content": "Hi",
        "created_at": "..."
      },
      {
        "id": "uuid",
        "role": "assistant",
        "content": "Hello!",
        "created_at": "..."
      }
    ]
    ```

### 3. Configuration & Accounts

#### Check Onboarding Status
*   **GET** `/api/onboarding`
*   **Description:** Checks status of all registered integrations.
*   **Response:**
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
           "fields": [ ... ]
        }
      }
    ]
    ```

#### List Configured Accounts
*   **GET** `/api/config/accounts`
*   **Description:** Lists all accounts stored in the database or config.
*   **Response:**
    ```json
    [
      {
        "id": "uuid-or-default",
        "integration": "google",
        "name": "Personal Gmail",
        "details": { "email": "..." }
      },
      {
        "id": "uuid",
        "integration": "email",
        "name": "Work Email",
        "details": { "username": "..." }
      }
    ]
    ```

#### Add Account
*   **POST** `/api/config/accounts`
*   **Description:** Add a new account configuration.
*   **Request Body:**
    ```json
    {
      "integration": "email", // or "google"
      "name": "My Account Name",
      "config": {
        // Integration specific fields
        // For Email:
        "username": "me@example.com",
        "password": "secret_password",
        "imap_host": "imap.example.com",
        "imap_port": 993,
        "smtp_host": "smtp.example.com",
        "smtp_port": 587
        
        // For Google (if manually adding token):
        // "refresh_token": "..."
      }
    }
    ```
*   **Response:** `string` (The ID of the created account).

#### Delete Account
*   **DELETE** `/api/config/accounts/{id}`
*   **Description:** Remove an account configuration.

### 4. Setup

#### Google OAuth Start
*   **GET** `/setup/google`
*   **Description:** Redirects browser to Google OAuth consent screen.
*   **Note:** The callback will hit `/oauth/callback` (public) which saves the token as the *default* account.

---

## WebSocket API

*   **URL:** `/api/chat/stream`
*   **Authentication:** Use query param `?token=...`

The WebSocket allows for real-time streaming of the agent's response.

### Client -> Server
*   **Send Message:**
    ```json
    {
      "message": "Hello",
      "conversation_id": "optional-uuid" 
    }
    ```

### Server -> Client (Events)

1.  **Delta (Streaming Token):**
    ```json
    {
      "type": "delta",
      "content": "He"
    }
    ```
    ```json
    {
      "type": "delta",
      "content": "llo"
    }
    ```

2.  **Tool Execution Result:**
    ```json
    {
      "type": "tool_result",
      "tool": "google_search",
      "result": "Search results JSON..."
    }
    ```

3.  **Done (Stream Finished):**
    ```json
    {
      "type": "done",
      "conversation_id": "uuid"
    }
    ```

4.  **Error:**
    ```json
    {
      "type": "error",
      "error": "Something went wrong"
    }
    ```
