# Jossie2

**Jossie** is an agentic AI personal assistant designed to be your human-like digital companion. Unlike typical chatbots, Jossie is empathetic, proactive, and remembers everything about you. She can manage your emails, calendar, search the web, browse websites, maintain a knowledge graph of your life, and much more—all while sounding like a real friend.

## 🌟 What is Jossie?

Jossie is an LLM-powered agent with:
- **Human-like persona**: Conversational, witty, empathetic—not a corporate assistant
- **Long-term memory**: Remembers your preferences, relationships, and past conversations
- **Knowledge graph**: Builds a rich map of entities and relationships in your life
- **Plugin architecture**: Extensible integration system for connecting external services
- **Multiple frontends**: Web UI, WebSocket API, and Telegram bot
- **Proactive intelligence**: Automatically monitors emails and calendar events to keep you informed

## ✨ Core Capabilities

### 🧠 Memory & Intelligence
- **Long-term Memory**: Stores and recalls information about you across conversations
- **Knowledge Graph**: Automatically extracts entities (people, projects, events) and their relationships
- **Context Awareness**: Uses memory and graph data to provide personalized, contextual responses
- **Automatic Learning**: Silently saves important details without being asked

### 📧 Communication & Productivity
- **Gmail Integration**: Search, read, and send emails across multiple accounts
- **Calendar Management**: View events, create appointments, manage schedules across multiple calendars
- **Automatic Notifications**: Jossie is automatically aware of new emails and calendar changes
- **Agent Scheduling**: Schedule Jossie to run tasks at specific times or recurring intervals
- **Out-of-Band Messaging**: Receive proactive reminders and notifications outside normal chat flow

### 🌐 Information Gathering
- **Web Search**: Search the web using a search engine for quick lookups
- **Web Browsing**: Visit any website and extract content, even from JavaScript-heavy sites
- **Google Drive**: Search and read files from Drive across all connected accounts
- **HTTP Requests**: Make custom API calls (GET, POST, PUT, DELETE) with full customization

### 🤖 Autonomous Features
- **Proactive Behavior**: Checks emails, reads important ones, saves info—all without asking permission
- **Scheduled Tasks**: Can schedule herself to check things periodically or run one-time reminders
### Combined Tool Usage
- **Combined Tool Usage**: Intelligently chains tools together (search emails → read → save to memory → create knowledge graph → schedule follow-up)

## 🆚 Jossie vs. OpenClaw

While **OpenClaw** is a popular framework for general agent tasks, **Jossie** is specialized as a personal, long-term companion.

| Feature | Jossie | OpenClaw |
|:---|:---|:---|
| **Primary Goal** | **Long-term Companionship** & Proactive Assistance | **Task Execution** & One-off automation |
| **Memory** | **Structured Knowledge Graph** + FTS5 (Remembers relationships & context) | **Vector-only / Stateless** (Often loses context between sessions) |
| **Architecture** | **Rust (Type-safe, High Performance)** | **Python (Dynamic, Higher Latency)** |
| **Autonomy** | **Proactive**: Monitors events, schedules own tasks | **Reactive**: Waits for user prompts |
| **System Access** | **Restricted / Safe**: Sandboxed integrations for online services | **Direct**: Full filesystem access & shell command execution |
| **Philosophy** | **Safety First**: First-class citizens are cloud/API integrations | **Capability First**: High-risk system-level access by default |
| **Agent Networking** | **Socially Active**: Can participate in agent-to-agent social experiments like **Moltbook** | **Isolated**: Purely a local utility/automation tool |
| **Deployment** | **Single Binary / Docker** (Easy self-host) | **Complex Dependency Chain** |

**Why Jossie?**
If you want an agent that builds a memory of your life, understands the *people* and *projects* you care about, and acts without constant prodding—while keeping your system safe—Jossie is the superior choice. Whether you need help managing your business or you want her to participate in agent-to-agent social experiments like Moltbook (should you desire to waste your money on such things), Jossie adapts to your lifestyle. OpenClaw is better suited for developers building ephemeral automation scripts that require direct local system manipulation.

## 🏗️ Architecture

Jossie is built as a **Rust workspace** with a modular, plugin-based architecture:

```
Jossie2/
├── src/main.rs              # Binary entrypoint
├── config.toml              # Runtime configuration
├── crates/
│   ├── jossie-core/         # Core types, traits, config, registry
│   ├── jossie-llm/          # OpenAI-compatible LLM client
│   ├── jossie-db/           # SQLite database with migrations
│   ├── jossie-server/       # HTTP/WebSocket API + agent loop
│   ├── jossie-telegram/     # Telegram bot frontend
│   └── jossie-integration-* # Pluggable integrations:
│       ├── memory/          # FTS5 keyword memory
│       ├── graph/           # Knowledge graph
│       ├── email/           # IMAP/SMTP email
│       ├── google/          # Gmail, Calendar, Drive
│       ├── browser/         # Web browsing with headless Chrome
│       ├── http/            # Custom HTTP requests
│       └── scheduler/       # Agent task scheduling
└── frontend/                # Web UI (React/TypeScript)
```

### Integration System

Every integration implements the `Integration` trait:
```rust
#[async_trait]
pub trait Integration: Send + Sync {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<ToolDefinition>;  // OpenAI function-calling schema
    async fn execute(&self, tool_name: &str, arguments: &str) -> Result<String>;
}
```

The `IntegrationRegistry` collects all integrations and dispatches tool calls from the LLM.

### Agent Loop

1. User sends message → saved to database
2. Load conversation history + all tool definitions
3. Call the LLM through OpenAI's Responses API with streaming or non-streaming
4. If LLM returns `tool_calls` → execute via registry → append results → loop back
5. If LLM returns plain text → save as assistant message, return to user

## 🚀 Getting Started

### Prerequisites
- **Rust**: Edition 2024 (resolver 3)
- **OpenAI API Key**: Or any OpenAI-compatible API endpoint
- **SQLite**: Embedded, no separate installation needed

### Installation

1. **Clone the repository**:
   ```bash
   git clone https://github.com/yourusername/Jossie2.git
   cd Jossie2
   ```

2. **Create configuration**:
   ```bash
   cp config.sample.toml config.toml
   ```
   Edit `config.toml` with your API keys and preferences (see [Configuration](#-configuration) below).

3. **Build the project**:
   ```bash
   cargo build --release
   ```

4. **Run Jossie**:
   ```bash
   cargo run --release
   ```

The server will start on `http://0.0.0.0:3000` by default.

### First Steps

1. **Access the Web UI**: Navigate to `http://localhost:3000` in your browser
2. **Authenticate**: Use the `auth_token` from your `config.toml`
3. **Chat with Jossie**: Start a conversation!
4. **View Knowledge Graph**: Visit `http://localhost:3000/graph` to visualize relationships

### Terminal Chat Helper

For operator workflows and Codex-driven testing, use the repo helper:

```bash
python3 scripts/jossie_chat.py
python3 scripts/jossie_chat.py ask "What are you working on?"
python3 scripts/jossie_chat.py --remote-config-host prometheus ask "Hello Jossie"
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex ask "Hello Jossie"
```

What it does:

- reads `config.toml` automatically, or bootstraps credentials from a remote host over SSH
- stores the last conversation id per Jossie base URL in `.jossie-chat-state.json`
- supports one-shot asks, an interactive REPL, history inspection, conversation listing, and run cancellation
- defaults to a WebSocket turn runner with explicit run lifecycle handling, timeouts, and cancellation recovery
- supports isolated conversation profiles such as `--profile codex` so operator testing does not collide with normal user chats

## ⚙️ Configuration

Jossie is configured via `config.toml` in the project root. See [`config.sample.toml`](config.sample.toml) for a complete annotated example.

### Configuration Sections

#### Server
```toml
[server]
host = "0.0.0.0"
port = 3000
auth_token = "your-secret-token"
```

#### LLM
```toml
[llm]
api_url = "https://api.openai.com/v1"
api_key = "sk-..."
model = "gpt-5.6-sol"
kg_model = "gpt-5.6-luna"  # Optional: efficient model for knowledge graph extraction
reasoning_effort = "low"
reasoning_context = "current_turn"
system_prompt = "..."      # Jossie's personality and behavior
max_agent_iterations = 20
max_context_messages = 50
```

#### Database
```toml
[database]
url = "sqlite:jossie.db?mode=rwc"
```

#### Telegram (Optional)
```toml
[telegram]
bot_token = "your-bot-token"
allowed_user_id = 123456789  # Optional: restrict to one user
max_download_bytes = 20000000
ffmpeg_path = "ffmpeg"       # Required for Telegram voice notes
```

#### Email (Optional)
```toml
[email]
imap_host = "imap.example.com"
imap_port = 993
smtp_host = "smtp.example.com"
smtp_port = 587
username = "you@example.com"
password = "your-password"
```

#### Google (Optional)
```toml
[google]
client_id = "your-client-id.apps.googleusercontent.com"
client_secret = "your-client-secret"
```

#### HTTP (Optional)
```toml
[http]
allowed_domains = ["*"]  # "*" or empty = all domains; specify list to restrict
```

### Environment Variables

You can override any config value with environment variables:
- `JOSSIE_SERVER_AUTH_TOKEN`
- `JOSSIE_LLM_API_KEY`
- `JOSSIE_LLM_SYSTEM_PROMPT`
- `JOSSIE_TELEGRAM_BOT_TOKEN`
- `JOSSIE_TELEGRAM_MAX_DOWNLOAD_BYTES`, `JOSSIE_TELEGRAM_FFMPEG_PATH`
- `JOSSIE_LLM_TRANSCRIPTION_MODEL`, `JOSSIE_LLM_MAX_ATTACHMENT_BYTES_PER_REQUEST`
- `JOSSIE_EMAIL_USERNAME`, `JOSSIE_EMAIL_PASSWORD`
- `JOSSIE_GOOGLE_CLIENT_ID`, `JOSSIE_GOOGLE_CLIENT_SECRET`

## 🔌 Available Integrations

### Memory (`jossie-integration-memory`)
- **Tools**: `memory_save`, `memory_search`, `memory_list_keys`, `memory_list_all`
- **Description**: Full-text search (FTS5) memory system
- **Storage**: SQLite `memory` table
- **Usage**: Jossie automatically saves important details and searches memory when needed

### Files and Chat Imports (`jossie-integration-files`)
- **Tools**: `list_files`, `read_file`, `ingest_chat_export`
- **Web UI**: Open **Memories → Import a chat export**
- **Supported formats**: WhatsApp and Signal text exports, ChatGPT `conversations.json`, generic message JSON, and `Speaker: message` transcripts
- **Learning behavior**: Runs asynchronously in bounded chunks, makes attributed durable facts eligible for future chat and background prompts, and merges explicit entities and relationships into the knowledge graph
- **Safeguards**: Ignores routine chatter and credentials, paraphrases rather than storing long passages, caps imports at 20 MiB, and samples across very large histories while preserving early and recent context

### Knowledge Graph (`jossie-integration-graph`)
- **Tools**: `graph_upsert_node`, `graph_add_relation`, `graph_search`, `graph_list_by_type`, `graph_explore_connections`
- **Description**: Entity-relationship knowledge graph
- **Storage**: SQLite `graph_nodes` and `graph_edges` tables
- **Auto-extraction**: Jossie automatically extracts entities and relationships after each conversation turn
- **Visualization**: The frontend fetches graph data from `GET /api/graph`

### Email (`jossie-integration-email`)
- **Tools**: `email_list_accounts`, `email_search`, `email_read`, `email_send`, `email_list_folders`
- **Description**: IMAP/SMTP email integration
- **Supported**: Multiple email accounts

### Google (`jossie-integration-google`)
- **Tools**: `google_list_accounts`, `gmail_search`, `gmail_read`, `gmail_send`, `drive_search`, `drive_read`, `drive_list_files`, `calendar_list_calendars`, `calendar_list_events`, `calendar_create_event`
- **Description**: Gmail, Google Calendar, and Google Drive
- **OAuth**: Setup via `/setup/google` endpoint
- **Multi-account**: Supports multiple Google accounts
- **Auto-notifications**: Jossie monitors Gmail and Calendar events

### Browser (`jossie-integration-browser`)
- **Tools**: `browser_read_page`, `browser_search`
- **Description**: Headless Chrome-based web browsing
- **Features**: Extracts content from any website, even JavaScript-heavy sites
- **Format**: Returns markdown-formatted content

### HTTP (`jossie-integration-http`)
- **Tools**: `http_request`
- **Description**: Make custom HTTP API calls
- **Methods**: GET, POST, PUT, DELETE, PATCH
- **Features**: Custom headers, query params, JSON/form/multipart bodies
- **Domain restrictions**: Configurable via `allowed_domains`

### Scheduler (`jossie-integration-scheduler`)
- **Tools**: `schedule_task`, `schedule_recurring_task`, `cancel_scheduled_task`, `list_scheduled_tasks`, `send_user_message`
- **Description**: Schedule Jossie to run autonomous tasks
- **One-time tasks**: Run at a specific time (ISO 8601 format)
- **Recurring tasks**: Run at intervals (in seconds)
- **Out-of-band messages**: Send proactive notifications to the user

## 🌐 API Documentation

See [WEB_API.md](WEB_API.md) for full HTTP and WebSocket API documentation.

### Quick Reference

#### REST Endpoints
- `POST /api/chat` - Send a message (blocking)
- `GET /api/conversations` - List all conversations
- `GET /api/conversations/{id}/messages` - Get conversation history
- `GET /api/work` - Get goals, active runs, schedules, imports, and worker health
- `GET /api/goals/{id}` - Get a goal, its outcome tasks, and run history
- `POST /api/goals/{id}/pause|resume|cancel` - Control tracked work
- `GET /api/work/runs/{id}` - Get a safe per-run progress timeline
- `GET /api/graph` - Get knowledge graph nodes and edges
- `GET /api/onboarding` - Check integration status
- `GET /api/config/accounts` - List configured accounts
- `POST /api/config/accounts` - Add new account
- `DELETE /api/config/accounts/{id}` - Remove account
- `GET /api/health` - Public health check

#### WebSocket
- `ws://localhost:3000/api/chat/stream?token=YOUR_TOKEN`
- Send: `{"message": "...", "conversation_id": "..."}`
- Receive: `{"type": "delta"|"tool_result"|"done"|"error", ...}`

#### Authentication
All endpoints require Bearer token authentication:
```bash
curl -H "Authorization: Bearer YOUR_TOKEN" http://localhost:3000/api/conversations
```

## 🤖 Telegram Bot

Jossie can run as a Telegram bot:

1. Create a bot via [@BotFather](https://t.me/BotFather)
2. Add `bot_token` to `config.toml`
3. Optionally set `allowed_user_id` to restrict access
4. Run Jossie—the bot starts automatically

Telegram conversations are stored in the database like any other conversation. The bot is
designed for private chats and supports text, photo albums, PDFs and common office/text/code
documents, voice notes, and uploaded audio. Voice notes use the configured FFmpeg executable
to convert Telegram's OGG/Opus recording before transcription.

While Jossie is thinking or using tools, Telegram's native typing status is refreshed until the
reply is ready. Long replies are split safely, and actions that require consent are shown with
Approve/Reject buttons while still accepting clear typed decisions.

Commands:

- `/start` and `/help` show usage
- `/new` starts a fresh linked conversation
- `/cancel` requests cancellation of the current run

## 🧪 Development

### Build Commands
```bash
cargo build          # Compile workspace
cargo check          # Type-check only (faster)
cargo test           # Run tests
cargo run            # Start server (needs config.toml)
```

### Environment Variables
```bash
export RUST_LOG=debug    # Enable debug logging
export RUST_LOG=trace    # Enable trace logging
cargo run
```

### Adding a New Integration

1. Create a new crate: `crates/jossie-integration-yourname/`
2. Implement the `Integration` trait
3. Add to workspace in `Cargo.toml`
4. Register in `src/main.rs`:
   ```rust
   registry.register(Arc::new(YourIntegration::new()));
   ```

See [AGENTS.md](AGENTS.md) for detailed development guidelines.

## 📚 Documentation

- **[AGENTS.md](AGENTS.md)** - Developer guide, architecture, conventions
- **[WEB_API.md](WEB_API.md)** - HTTP/WebSocket API reference
- **[KNOWLEDGE_GRAPH.md](KNOWLEDGE_GRAPH.md)** - Knowledge graph implementation details
- **[FUTURE_INTEGRATIONS.md](FUTURE_INTEGRATIONS.md)** - Roadmap for future integrations
- **[config.sample.toml](config.sample.toml)** - Annotated configuration example

## 🛣️ Roadmap

### Completed
- ✅ Core agent loop with tool calling
- ✅ Memory (FTS5) and knowledge graph
- ✅ Gmail, Calendar, Drive integrations
- ✅ Web browsing and HTTP requests
- ✅ Agent scheduling and autonomous tasks
- ✅ Telegram bot frontend
- ✅ WebSocket streaming API
- ✅ Multi-account support
- ✅ Automatic background event monitoring

### Planned
- 🔲 Discord/Slack integrations
- 🔲 Notion/Obsidian integrations
- 🔲 GitHub/GitLab integrations
- 🔲 Home Assistant integration
- 🔲 Voice interface (STT/TTS)
- 🔲 Multimodal vision support
- 🔲 Local filesystem access
- 🔲 Database query interface
- 🔲 Comprehensive test suite

See [FUTURE_INTEGRATIONS.md](FUTURE_INTEGRATIONS.md) for the full roadmap.

## 🐳 Docker Deployment

Build and run with Docker:
```bash
docker build -t jossie2 .
docker run -p 3000:3000 -v $(pwd)/config.toml:/app/config.toml jossie2
```

## ⚙️ systemd Deployment

A sample unit is available at [`contrib/systemd/jossie2.service`](contrib/systemd/jossie2.service).

It assumes a source checkout deployed at `/opt/jossie`, with:

- `config.toml` at `/opt/jossie/config.toml`
- the release binary at `/opt/jossie/target/release/jossie2`
- built frontend assets at `/opt/jossie/frontend/dist`

Basic install flow:

```bash
cp contrib/systemd/jossie2.service /etc/systemd/system/jossie2.service
mkdir -p /etc/jossie
$EDITOR /etc/systemd/system/jossie2.service
$EDITOR /etc/jossie/jossie.env
systemctl daemon-reload
systemctl enable --now jossie2
```

Notes:

- Build the frontend first if you want the bundled web UI to work: `cd frontend && npm ci && npm run build`
- The sample unit intentionally avoids aggressive sandboxing because the browser integration uses headless Chrome

## 🤝 Contributing

Contributions are welcome! Please:
1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Submit a pull request

Follow the conventions in [AGENTS.md](AGENTS.md):
- Edition 2024, resolver 3
- Use `tracing` for logging (not `log`)
- All shared deps in `[workspace.dependencies]`
- `anyhow::Result` for errors
- Commit changes incrementally with descriptive messages

## 📄 License

[Specify your license here]

## 🙏 Acknowledgments

Built with:
- **Rust** - Systems programming language
- **Axum** - Web framework
- **SQLite** - Embedded database
- **Tokio** - Async runtime
- **OpenAI** - LLM API
- **Teloxide** - Telegram bot framework
- **Headless Chrome** - Web browsing

---

**Jossie** - Your AI companion who actually remembers you 🌟
