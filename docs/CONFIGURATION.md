# HydeClaw Configuration Guide

This guide covers every configuration file, environment variable, workspace convention, and deployment target for HydeClaw.

---

## Table of Contents

1. [Environment Variables (.env)](#environment-variables-env)
2. [Main Config (config/hydeclaw.toml)](#main-config-confighydeclawttoml)
3. [Agent Config (config/agents/{Name}.toml)](#agent-config-configagentsnametoml)
4. [Docker Deployment](#docker-deployment)
5. [Workspace Structure](#workspace-structure)
6. [Database Migrations](#database-migrations)
7. [Managed Processes](#managed-processes)
8. [Watchdog (config/watchdog.toml)](#watchdog-configwatchdogtoml)
9. [Makefile Targets](#makefile-targets)

---

## Environment Variables (.env)

The `.env` file is read by the systemd service unit at startup. **Only three variables belong here.** Every other secret (API keys, bot tokens, provider credentials) is stored in the encrypted secrets vault and managed through the UI or API.

Copy `.env.example` to `.env` and fill in all three values before starting the service.

| Variable | Required | Description |
|---|---|---|
| `HYDECLAW_AUTH_TOKEN` | Yes | Bearer token for all API requests. Used by the UI, Makefile targets, and channel adapters. |
| `HYDECLAW_MASTER_KEY` | Yes | 32-byte key for ChaCha20-Poly1305 encryption of the secrets vault. Never changes after first run. |
| `DATABASE_URL` | Yes | PostgreSQL connection string. Must match `[database].url` in `hydeclaw.toml`. |

### Generating Each Value

**HYDECLAW_AUTH_TOKEN** — any long random string. Generate with:
```bash
openssl rand -hex 32
```

**HYDECLAW_MASTER_KEY** — must be exactly 32 bytes, base64-encoded:
```bash
openssl rand -base64 32
```
Write this value down. If it is lost, all vault secrets become unrecoverable and must be re-entered.

**DATABASE_URL** — standard libpq connection string:
```
postgresql://hydeclaw:yourpassword@localhost:5432/hydeclaw
```
For Docker deployments where PostgreSQL runs in a container, use the container's service name as the host (e.g. `postgres`).

### Policy: Secrets Vault vs .env

`.env` holds only what the process needs before it can connect to the database. Everything else goes through `POST /api/secrets`:

```bash
curl -X POST http://localhost:18789/api/secrets \
  -H "Authorization: Bearer $HYDECLAW_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name": "BRAVE_SEARCH_API_KEY", "value": "BSA1...", "scope": ""}'
```

Per-agent secrets use `"scope": "AgentName"`. The lookup order is: agent-scoped → global → environment variable fallback.

---

## Main Config (config/hydeclaw.toml)

The primary runtime configuration file. Core reads it at startup; most values require a restart to take effect.

### [gateway]

Controls the HTTP server and authentication.

| Field | Type | Default | Description |
|---|---|---|---|
| `listen` | string | `"0.0.0.0:18789"` | Bind address and port for the HTTP server. |
| `auth_token_env` | string | `"HYDECLAW_AUTH_TOKEN"` | Name of the environment variable that holds the bearer token. Change this only if you rename the `.env` key. |
| `public_url` | string | — | External URL where this instance is reachable. Used to construct webhook callbacks and Telegram webhook URLs. Example: `"http://your-server:18789"`. |

```toml
[gateway]
listen = "0.0.0.0:18789"
auth_token_env = "HYDECLAW_AUTH_TOKEN"
public_url = "http://your-server:18789"
```

### [database]

| Field | Type | Description |
|---|---|---|
| `url` | string | PostgreSQL connection URL. Should match `DATABASE_URL` in `.env`. sqlx uses this for the connection pool. |

```toml
[database]
url = "postgresql://hydeclaw:hydeclaw@localhost:5432/hydeclaw"
```

### [limits]

Rate limiting and concurrency controls applied globally across all agents.

| Field | Type | Default | Description |
|---|---|---|---|
| `max_requests_per_minute` | integer | `100` | Maximum number of incoming requests per minute across the entire gateway. Requests over this limit receive HTTP 429. |
| `max_tool_concurrency` | integer | `10` | Maximum number of tool calls executing simultaneously across all agent sessions. Enforced by a tokio `Semaphore`. Prevents runaway tool loops from starving other sessions. |
| `request_timeout_secs` | integer | `180` | Maximum duration for a single API request in seconds. Requests exceeding this limit are terminated. |
| `max_agent_turns` | integer | `5` | Maximum agent-to-agent turns in a single request. Prevents infinite delegation loops between agents. |
| `max_handoff_context_chars` | integer | `2000` | Maximum context size (in characters) for inter-agent messages (legacy field name, still used in config API). Longer contexts are truncated. |

```toml
[limits]
max_requests_per_minute = 100
max_tool_concurrency = 10
request_timeout_secs = 180
max_agent_turns = 5
max_inter_agent_context_chars = 2000  # limit context transfer size between agents
```

### [typing]

Controls the "typing..." indicator sent to channel users while the agent is thinking.

| Field | Type | Default | Description |
|---|---|---|---|
| `mode` | string | `"instant"` | `"instant"` — typing indicator sent immediately when a message arrives. `"none"` — disabled. `"typing"` — classic typing animation where supported. |

```toml
[typing]
mode = "instant"
```

### [subagents]

Configuration for the live agent pool system. Controls whether the `agent` tool can spawn live agents to delegate sub-tasks.

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. When false, `agent` tool calls are rejected. |
| `default_mode` | string | `"in-process"` | Where live agents run by default. `"in-process"` runs them as tokio tasks inside Core. `"docker"` runs them in a sandbox container. |
| `max_concurrent_in_process` | integer | `5` | Maximum simultaneously running in-process sub-agents. |
| `max_concurrent_docker` | integer | `3` | Maximum simultaneously running Docker sandbox sub-agents. |
| `docker_timeout` | string | `"5m"` | Maximum lifetime for a Docker sub-agent. Human-readable duration: `"5m"`, `"30s"`, `"2h"`. |
| `in_process_timeout` | string | `"2m"` | Maximum lifetime for an in-process sub-agent. |

```toml
[subagents]
enabled = true
default_mode = "in-process"
max_concurrent_in_process = 5
max_concurrent_docker = 3
docker_timeout = "5m"
in_process_timeout = "2m"
```

### [docker]

Controls how Core interacts with Docker for managing infrastructure services (via bollard over TCP at `tcp://127.0.0.1:2375`).

| Field | Type | Description |
|---|---|---|
| `compose_file` | string | Path to the Docker Compose file relative to the working directory. Default: `"docker/docker-compose.yml"`. |
| `rebuild_allowed` | array of strings | List of Compose service names that the API allows Core to rebuild (e.g. `["browser-renderer", "searxng"]`). Services not in this list can be restarted but not rebuilt via the API. |
| `rebuild_timeout_secs` | integer | Maximum seconds to wait for a `docker compose build` operation before aborting. Default: `300`. |

```toml
[docker]
compose_file = "docker/docker-compose.yml"
rebuild_allowed = ["browser-renderer", "searxng"]
rebuild_timeout_secs = 300
```

### [[managed_process]] — channels

The channel adapter is a TypeScript/Bun process that runs as a native OS process managed by Core (not in Docker). Core spawns it at startup, monitors its health URL, and restarts it if it fails.

| Field | Type | Description |
|---|---|---|
| `name` | string | Internal name. Must be `"channels"`. Used in API endpoints like `POST /api/services/channels/restart`. |
| `command` | array of strings | Command line to launch the process. Default: `["bun", "run", "src/index.ts"]`. |
| `working_dir` | string | Directory where the command runs. Relative to the Core working directory. Default: `"channels"`. |
| `env_passthrough` | array of strings | Environment variable names to forward from Core's environment into the child process. |
| `env_extra` | map | Additional environment variables set for the child process only. `${VAR}` syntax expands values from Core's environment. |
| `health_url` | string | URL Core polls to determine if the process is healthy. HTTP 200 = healthy. |
| `port` | integer | Port the process listens on. Informational — used in health checks and log messages. |
| `memory_max` | string | Soft memory limit for cgroup-based limiting if supported. Example: `"256M"`. |

```toml
[[managed_process]]
name = "channels"
command = ["bun", "run", "src/index.ts"]
working_dir = "channels"
env_passthrough = ["HYDECLAW_AUTH_TOKEN"]
env_extra = { HYDECLAW_CORE_WS = "ws://localhost:18789", HEALTH_PORT = "3100" }
health_url = "http://localhost:3100/health"
port = 3100
memory_max = "256M"
```

### [[managed_process]] — toolgate

Toolgate is a Python/FastAPI media hub providing STT, Vision, TTS, and image generation via swappable provider drivers.

| Field | Type | Description |
|---|---|---|
| `name` | string | Internal name. Must be `"toolgate"`. |
| `command` | array of strings | uvicorn invocation. The virtual environment path must be absolute or relative to `working_dir`. |
| `working_dir` | string | Directory containing `app.py`. Default: `"toolgate"`. |
| `env_passthrough` | array of strings | External API credentials to pass from Core's environment (e.g. `WHISPER_URL`, `VISION_URL`, `TTS_BACKEND_URL`). These are typically set as global secrets and exported before launch. |
| `env_extra` | map | Runtime config for toolgate itself. `AUTH_TOKEN` gates toolgate's own API. `INTERNAL_NETWORK` CIDR for trusted callers. `CONFIG_PATH` points to the provider registry JSON. |
| `health_url` | string | Toolgate health endpoint. Returns `{"status":"ok",...}` when all configured providers are reachable. |
| `port` | integer | `9011`. |
| `memory_max` | string | `"256M"`. |

```toml
[[managed_process]]
name = "toolgate"
command = [".venv/bin/python", "-m", "uvicorn", "app:app", "--host", "0.0.0.0", "--port", "9011", "--workers", "1", "--loop", "asyncio"]
working_dir = "toolgate"
env_passthrough = ["WHISPER_URL", "VISION_URL", "VISION_MODEL", "OLLAMA_API_KEY", "TTS_BACKEND_URL", "MINIMAX_API_KEY"]
env_extra = { AUTH_TOKEN = "${HYDECLAW_AUTH_TOKEN}", INTERNAL_NETWORK = "127.0.0.0/8", CONFIG_PATH = "providers.json" }
health_url = "http://localhost:9011/health"
port = 9011
memory_max = "256M"
```

### [sandbox]

Code execution sandbox for regular (non-base (sandboxed)) agents. Runs arbitrary code inside an isolated Docker container.

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | When false, `code_exec` tool calls from non-base (sandboxed) agents are rejected. |
| `image` | string | `"hydeclaw-sandbox:latest"` | Docker image to use for the sandbox container. Build it from `docker/Dockerfile.sandbox`. |
| `extra_binds` | array of strings | `[]` | Additional volume mounts in `host:container` format. Use to give sandbox access to specific directories on the host. |
| `timeout_secs` | integer | `30` | Execution timeout in seconds before the sandbox container is killed. |
| `memory_mb` | integer | `256` | Memory limit per sandbox execution in megabytes. |

```toml
[sandbox]
enabled = true
image = "hydeclaw-sandbox:latest"
extra_binds = []
timeout_secs = 30
memory_mb = 256
```

Privileged agents (those with `base = true` in their agent config) bypass the sandbox and run `code_exec` directly on the host. Use this only for trusted system management agents.

### [memory]

Vector embedding settings for long-term memory.

| Field | Type | Description |
|---|---|---|
| `embed_dim` | integer | Embedding dimension. **Auto-detected at startup** by querying the configured embedding endpoint. Only set this manually to override auto-detection. Example: `2560` for `qwen3-embedding:4b`. |

```toml
[memory]
# embed_dim = 2560  # auto-detected at startup
```

The embedding endpoint is configured via the toolgate provider registry (`providers.json`). Memory uses cosine similarity search with MMR reranking.

### [memory_worker]

The memory worker is a separate binary (`hydeclaw-memory-worker`) that runs as its own systemd service. It handles background embedding, extraction queue processing, and GraphRAG entity extraction without blocking the main Core process.

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Whether Core expects the memory worker to be running. When true, the worker's status appears in `/api/doctor`. |
| `poll_interval_secs` | integer | `5` | How often (in seconds) the worker polls the extraction queue for new items to process. |

```toml
[memory_worker]
enabled = true
poll_interval_secs = 5
```

---

## Agent Config (config/agents/{Name}.toml)

Each file in `config/agents/` defines one agent. The filename (without `.toml`) is the agent's name. Names are case-sensitive. Create as many agents as needed — each gets its own conversation history, memory, tool permissions, and channel connections.

### [agent]

Core identity and LLM settings.

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | Yes | Agent name. Must match the filename. Used as the agent identifier in the API, database, and secret scoping. |
| `language` | string | No | Primary language for the agent's responses. Example: `"ru"`, `"en"`. Passed as context to skills and channel adapters. |
| `provider` | string | Yes | LLM provider type. `"openai"`, `"anthropic"`, `"minimax"`, or any OpenAI-compatible provider name configured in `providers`. |
| `model` | string | Yes | Model identifier for the chosen provider. Example: `"gpt-4o-mini"`, `"minimax-m2.7"`, `"claude-sonnet-4-5"`. |
| `temperature` | float | No | Sampling temperature (0.0–2.0). Lower = more deterministic. Default varies by provider. |
| `routing` | array of strings | No | List of Telegram user IDs or patterns that are routed to this agent. Empty array means the agent accepts traffic from its own registered channels. |
| `base` | bool | No | System agent flag. When `true`: cannot be renamed or deleted via API, SOUL.md and IDENTITY.md are read-only, `code_exec` runs directly on the host (no Docker sandbox), can write to service source directories and use tools marked `required_base = true`. Default: `false`. |

```toml
[agent]
name = "Hyde"
language = "ru"
provider = "minimax"
model = "minimax-m2.7"
temperature = 0.5
routing = []
base = true
```

### [agent.access]

Access control for incoming messages.

| Field | Type | Description |
|---|---|---|
| `mode` | string | `"restricted"` — only `owner_id` can send messages. `"open"` — anyone who can reach a channel can send messages. |
| `owner_id` | string | Telegram user ID of the owner. Required when `mode = "restricted"`. The channels adapter checks this before forwarding messages to Core. Example: `"123456789"`. |

```toml
[agent.access]
mode = "restricted"
owner_id = "YOUR_TELEGRAM_USER_ID"
```

Find your Telegram user ID by messaging `@userinfobot` on Telegram.

### [agent.heartbeat]

Configures the agent's periodic self-triggered task, based on cron scheduling.

| Field | Type | Description |
|---|---|---|
| `cron` | string | Standard 5-field cron expression. Example: `"*/15 * * * *"` (every 15 minutes), `"0 10 * * *"` (daily at 10:00). |
| `timezone` | string | IANA timezone name. Example: `"UTC"`, `"Europe/Moscow"`, `"America/New_York"`. |

When the cron fires, Core sends a `HEARTBEAT` message to the agent. The agent's `HEARTBEAT.md` skill file describes what to do (backups, memory deduplication, reports, etc.).

```toml
[agent.heartbeat]
cron = "*/15 * * * *"
timezone = "UTC"
```

### [agent.tools]

Fine-grained control over which tools the agent can use.

| Field | Type | Description |
|---|---|---|
| `allow` | array of strings | Explicit allow list. Tool names listed here are permitted regardless of `allow_all`. |
| `deny` | array of strings | Explicit deny list. These tools are always blocked, even if `allow_all = true`. |
| `allow_all` | bool | When `true`, all loaded tools are available except those in `deny`. Default: `false`. |
| `deny_all_others` | bool | When `true` combined with specific `allow` entries, only the allowed tools are available and everything else is blocked. Useful for sandboxed agents. |

```toml
[agent.tools]
allow = []
deny = ["workspace_write"]
allow_all = true
deny_all_others = false
```

#### [agent.tools.groups]

Tool groups are named sets of built-in system tools. Setting a group to `true` grants access to all tools in that group.

| Group | Tools included |
|---|---|
| `git` | `git_status`, `git_diff`, `git_log`, `git_commit`, `git_push` |
| `tool_management` | `tool_discover`, `tool_test`, `tool_enable`, `tool_disable` |
| `skill_editing` | `workspace_edit` for skill files |
| `session_tools` | `session_list`, `session_get`, `session_delete` |

```toml
[agent.tools.groups]
git = true
tool_management = true
skill_editing = true
session_tools = true
```

### [agent.compaction]

Context window management. When the conversation grows long, Core compacts older messages into a summary to stay within the LLM's context limit.

| Field | Type | Description |
|---|---|---|
| `enabled` | bool | Enable automatic compaction. Default: `true`. |
| `threshold` | float | Fraction of the model's context window (0.0–1.0) at which compaction triggers. `0.8` = compact when 80% full. |
| `preserve_tool_calls` | bool | When `true`, tool call/result pairs are never removed during compaction — only pure conversation messages are summarized. |
| `preserve_last_n` | integer | Always keep the last N messages verbatim, regardless of compaction. |

```toml
[agent.compaction]
enabled = true
threshold = 0.8
preserve_tool_calls = true
preserve_last_n = 10
```

### [agent.session]

Session lifecycle settings.

| Field | Type | Description |
|---|---|---|
| `dm_scope` | string | How DM sessions are scoped. `"shared"` — all DM conversations with this agent share one session per user. `"per_channel"` — separate session per channel. |
| `ttl_days` | integer | Sessions older than this many days (since last message) are automatically archived. `0` = never expire. |
| `max_messages` | integer | Maximum messages to keep per session before the oldest are dropped. `0` = unlimited. |

```toml
[agent.session]
dm_scope = "shared"
ttl_days = 30
max_messages = 0
```

### [agent.tool_loop]

Guards against runaway tool-use loops where the agent calls tools repeatedly without producing a final answer.

| Field | Type | Description |
|---|---|---|
| `max_iterations` | integer | Hard limit on tool call iterations per request. When reached, the loop is broken and the agent is asked to summarize. |
| `compact_on_overflow` | bool | When `max_iterations` is reached, trigger context compaction before asking for a summary. |
| `detect_loops` | bool | Enable loop detection heuristics (same tool with same arguments called repeatedly). |
| `warn_threshold` | integer | After this many identical consecutive tool calls, add a warning to the system prompt. |
| `break_threshold` | integer | After this many identical consecutive tool calls, forcibly break the loop. |

```toml
[agent.tool_loop]
max_iterations = 50
compact_on_overflow = true
detect_loops = true
warn_threshold = 5
break_threshold = 10
```

### [agent.channel.telegram]

Enable Telegram channel participation for this agent. The bot token must be stored as a scoped secret with `name = "BOT_TOKEN"` and `scope = "AgentName"`.

| Field | Type | Description |
|---|---|---|
| `enabled` | bool | When `true`, Core registers this agent for the Telegram channel adapter at startup. |

```toml
[agent.channel.telegram]
enabled = true
```

After setting this, register the channel via the API to store the bot token:

```bash
curl -X POST http://localhost:18789/api/agents/MyAgent/channels \
  -H "Authorization: Bearer $HYDECLAW_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"channel_type": "telegram", "display_name": "My Bot",
       "config": {"bot_token": "1234567890:ABC..."}}'
```

The token is stored in the vault automatically. The `channels` managed process picks it up via the Core WebSocket and starts the Telegram polling loop.

---

## Docker Deployment

HydeClaw uses Docker Compose for infrastructure services. The `docker/docker-compose.yml` file defines all services. Core itself and the managed processes (channels, toolgate) run as native processes outside Docker.

### Infrastructure Services (always running)

| Service | Image | Port | Description |
|---|---|---|---|
| `postgres` | `hydeclaw-pg:17-age-pgvector` | `127.0.0.1:5432` | PostgreSQL 17 with pgvector extension. Stores sessions, messages, memory chunks, secrets, providers. Data in the `pgdata` named volume. |
| `searxng` | `searxng/searxng:latest` | `127.0.0.1:8080` | Private meta search engine. Used by the `search_web` YAML tool. Config in `docker/config/searxng/`. |
| `browser-renderer` | `browser-renderer:latest` (local build) | `127.0.0.1:9020` | Headless Chrome service for URL screenshot rendering. Used by the `screenshot_web` tool. |

### MCP Containers (on-demand)

MCP containers use the `profiles: ["on-demand"]` Compose profile. Core starts and stops them dynamically via bollard when agents call the corresponding MCP tool. They are not running by default.

| Service | Port | Description |
|---|---|---|
| `mcp-summarize` | `9002` | Text summarization via LLM. Requires `MINIMAX_API_KEY`. |
| `mcp-stock-analysis` | `9003` | Financial data analysis. |
| `mcp-weather` | `9004` | Weather data from Open-Meteo. |
| `mcp-obsidian` | `9005` | Obsidian vault access (mounts `../workspace`). |
| `mcp-github` | `9006` | GitHub API operations. Requires `GITHUB_TOKEN`. |
| `mcp-postgres` | `9007` | Direct SQL access to the HydeClaw database. |
| `mcp-browser-cdp` | `9030` | Browser automation via Chrome DevTools Protocol. |
| `mcp-fetch` | `9040` | HTTP fetch and content extraction. |
| `mcp-memory` | `9041` | Memory operations MCP bridge. |
| `mcp-sequential-thinking` | `9042` | Chain-of-thought reasoning tool. |
| `mcp-youtube-transcript` | `9043` | YouTube video transcript extraction. |
| `mcp-time` | `9044` | Timezone and time conversion. |
| `mcp-filesystem` | `9045` | Filesystem access (mounts `~/hydeclaw/workspace`). |
| `mcp-git` | `9046` | Git repository operations. |
| `mcp-notion` | `9048` | Notion API integration. |
| `mcp-todoist` | `9049` | Todoist task management. |

### docker/.env Variables

Create `docker/.env` (or set in the environment before running `docker compose up`):

| Variable | Required by | Description |
|---|---|---|
| `POSTGRES_USER` | `postgres`, `mcp-postgres` | PostgreSQL superuser name. Example: `hydeclaw`. |
| `POSTGRES_PASSWORD` | `postgres`, `mcp-postgres` | PostgreSQL superuser password. |
| `MINIMAX_API_KEY` | `mcp-summarize` | MiniMax API key for LLM-based summarization. |
| `GITHUB_TOKEN` | `mcp-github` | GitHub personal access token. |

### Starting Services

```bash
# Start infrastructure (postgres, searxng, browser-renderer)
cd docker && docker compose up -d

# Build and start all on-demand MCP images
cd docker && docker compose --profile on-demand build

# Start a specific service
docker compose up -d searxng

# View logs
docker compose logs -f postgres
```

---

## Workspace Structure

The `workspace/` directory is the runtime-editable part of HydeClaw. Changes here take effect without restarting Core (tools are hot-reloaded; skills and MCP configs are read on demand).

```
workspace/
├── tools/          # YAML HTTP tool definitions (hot-reloaded)
│   ├── _templates/ # Shared auth/header templates (extends:)
│   └── *.yaml
├── skills/         # Markdown behavioral guides (loaded per request)
│   └── *.md
├── agents/         # Per-agent workspace files
│   └── {Name}/
│       ├── SOUL.md       # Core behavior and security rules
│       ├── IDENTITY.md   # Personality and communication style
│       └── HEARTBEAT.md  # Heartbeat task instructions
└── mcp/            # MCP server registrations
    └── *.yaml
```

### workspace/tools/ — YAML Tool Format

Each `.yaml` file in `workspace/tools/` defines one HTTP tool available to agents.

**Required fields:**

| Field | Type | Description |
|---|---|---|
| `name` | string | Unique tool name in `snake_case`. Must be unique across all tool files. |
| `description` | string | English description for the LLM: when and why to call this tool. |
| `endpoint` | string | HTTP endpoint URL. Use `{param}` for path parameters (e.g. `https://api.example.com/v1/{id}`). |
| `method` | string | HTTP method: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`. |

**Parameters** — each parameter is a key under `parameters:`:

| Field | Default | Description |
|---|---|---|
| `type` | `"string"` | JSON Schema type: `string`, `integer`, `number`, `boolean`. |
| `required` | `false` | Whether the LLM must provide this parameter. |
| `location` | `"body"` | Where the parameter goes: `path`, `query`, `body`, `header`. |
| `description` | `""` | Description shown to the LLM. |
| `default` | null | Value used when the LLM does not provide one. |
| `default_from_env` | null | Secret/env var name to use as fallback before `default`. |
| `enum` | `[]` | Restrict to allowed values. |
| `minimum` / `maximum` | null | Numeric range constraints. |
| `examples` | `[]` | Example values appended to `description`. |

**Authentication** — the `auth:` block. The field is `type:` (not `auth_type:`):

| type | Extra fields | Description |
|---|---|---|
| `bearer_env` | `key` | `Authorization: Bearer $KEY`. `key` is the vault secret name. |
| `basic_env` | `username_key`, `password_key` | HTTP Basic auth from two vault secrets. |
| `api_key_header` | `header_name`, `key` | Custom header with value from vault secret. |
| `api_key_query` | `param_name`, `key` | Key appended as a query parameter. |
| `custom` | `headers` (map) | Static headers; use `${ENV_VAR}` to expand vault secrets in values. |
| `oauth_refresh` | `key`, `token_url`, `token_body`, `token_field` | OAuth2 client credentials / refresh flow. |
| `oauth_provider` | `key` | Use OAuth token from a connected integration. `key` is the provider name (e.g. `github`, `google`). |
| `none` | — | No authentication. |

**Additional fields:**

| Field | Default | Description |
|---|---|---|
| `status` | `"verified"` | `"verified"` (active), `"draft"` (loaded but not shown to LLM), `"disabled"` (not loaded). |
| `body_template` | null | Handlebars-style template for POST body. `{{param}}` substitution, `{{#if param}}...{{/if}}` conditionals. |
| `content_type` | `"application/json"` | Content-Type for POST requests. Also supports `multipart/form-data`, `application/x-www-form-urlencoded`. |
| `response_transform` | null | JSONPath string to extract from the response: `"$.results"`, `"$.data[*]"`, `"$.items[0:5]"`. |
| `response_pipeline` | null | Array of post-processing steps: `jsonpath`, `pick_fields`, `sort_by`, `limit`. |
| `channel_action` | null | After HTTP call, send binary data to the channel. Fields: `action` (`send_voice`, `send_photo`, `send_file`, `send_message`), `data_field` (`"_binary"` or JSONPath). |
| `headers` | `{}` | Static HTTP headers sent with every request. |
| `timeout` | `60` | Request timeout in seconds. |
| `retry` | null | `{max_attempts, backoff_base_ms, retry_on: [429, 500, 502, 503, 504]}`. |
| `rate_limit` | null | `{max_calls_per_minute}`. |
| `cache` | null | `{ttl, key_params}` — cache responses by parameter values. |
| `pagination` | null | `{type: offset/cursor/page, param, limit_param, limit, max_pages, results_path, next_path}`. |
| `graphql` | null | `{query, variables}` — GraphQL query with `{{param}}` templating. |
| `required_base` | `false` | When `true`, only base agents can call this tool. |
| `parallel` | `false` | Hint to the LLM that this tool can be called in parallel with others. |
| `extends` | null | Name of a template in `workspace/tools/_templates/` to inherit auth, headers, and base parameters from. |
| `tags` | `[]` | Labels for grouping and filtering tools in the UI. |

**Tool lifecycle:**
1. Create `workspace/tools/my_tool.yaml` with `status: draft`
2. Test from chat: `/tool_test my_tool {"param": "value"}`
3. Set `status: verified` — tool is now visible to the LLM
4. Set `status: disabled` — tool is excluded from loading

### workspace/skills/ — Skill Format

Skills are Markdown files with YAML frontmatter. They are injected into the agent's system prompt when triggered.

**Frontmatter fields:**

| Field | Required | Description |
|---|---|---|
| `name` | Yes | Unique skill identifier (`snake_case`). |
| `description` | Yes | When this skill is relevant (used for matching). |
| `triggers` | No | List of keywords/phrases that trigger automatic injection. |
| `tools_required` | No | Tool names this skill depends on. Listed for documentation; does not enforce availability. |
| `priority` | No | Integer. Higher priority skills are injected first when multiple match. |

```markdown
---
name: web_search_strategy
description: Web search strategy — when to use each search provider
triggers:
  - search
  - look it up
tools_required:
  - search_web
  - search_web_fresh
priority: 10
---

## Strategy content here...
```

### workspace/agents/{Name}/ — Agent Workspace Files

Per-agent workspace files are loaded into the agent's context automatically.

| File | Purpose |
|---|---|
| `SOUL.md` | Core behavioral rules, security policies, decision principles. Always injected into the system prompt. |
| `IDENTITY.md` | Personality, communication style, character definition. |
| `HEARTBEAT.md` | Instructions for what to do when the heartbeat cron fires. |

Additional Markdown files can be added and referenced by the agent via `workspace_read("agents/Name/filename.md")`.

### workspace/mcp/ — MCP Registration Format

Each `.yaml` file registers one MCP server. Core uses this to route `mcp_*` tool calls to the correct container.

| Field | Type | Description |
|---|---|---|
| `name` | string | MCP server identifier (matches Compose service name without `mcp-` prefix). |
| `container` | string | Docker container name (must match `container_name` in docker-compose.yml). |
| `port` | integer | Port the MCP HTTP server listens on inside the container. |
| `mode` | string | `"on-demand"` — container is started when first called and stopped after idle timeout. `"always"` — container stays running. |
| `idle_timeout` | string | How long after last call before the container is stopped. Example: `"5m"`. Only applies to `on-demand` mode. |
| `protocol` | string | `"http"` (HTTP/SSE-based MCP). |
| `enabled` | bool | When `false`, this MCP server is not available to agents. |

```yaml
name: mcp-github
container: mcp-github
port: 9006
mode: on-demand
idle_timeout: 5m
protocol: http
enabled: true
```

---

## Database Migrations

HydeClaw uses sequential numbered SQL migrations in the `migrations/` directory. Migrations run automatically at Core startup via sqlx. There is no manual migration step.

**Current state: 54 migrations.**

### Key Tables by Function

**Conversations:**

| Table | Description |
|---|---|
| `sessions` | Conversation sessions per agent/user/channel. Tracks `last_message_at` for TTL. |
| `messages` | Individual messages within sessions. Stores role, content, token counts, tool call metadata. |
| `pending_messages` | Outbound message queue for async delivery. |

**Memory:**

| Table | Description |
|---|---|
| `memory_chunks` | Long-term memory entries with pgvector embeddings and FTS indexes. Two-tier: raw (time-decayed) and pinned (permanent). |
| `memory_tasks` | Background job queue for the memory worker (reindex tasks). |

**Agents and Scheduling:**

| Table | Description |
|---|---|
| `scheduled_jobs` | Agent-created cron jobs (via the `cron` tool). Per-agent, with timezone support. |
| `cron_runs` | History of cron job executions. |
| `agent_goals` | Persistent goals with progress tracking and optional check schedule. |
| `agent_channels` | Registered channel connections per agent (type, config, status). |

**Secrets and Auth:**

| Table | Description |
|---|---|
| `secrets` | Encrypted key-value vault. PK is `(name, scope)`. ChaCha20-Poly1305 encryption using `HYDECLAW_MASTER_KEY`. |
| `providers` | Unified provider connections (LLM and media) with model, base URL, capabilities, and secret reference. |
| `oauth_connections` / `oauth_accounts` | OAuth2 integration tokens (GitHub, Google, etc.). |

**Tools and Audit:**

| Table | Description |
|---|---|
| `pending_approvals` | Tool calls awaiting owner confirmation. Used with the approval allowlist feature. |
| `tool_audit_log` | Record of every tool call: agent, tool name, args, result summary, duration. |
| `audit_events` | Broader audit trail for API actions (agent create/update/delete, secret writes). |
| `usage_log` | LLM token usage per session/agent. |

**Infrastructure:**

| Table | Description |
|---|---|
| `webhooks` | Registered inbound webhooks with per-webhook secret and agent routing. |
| `github_repos` | GitHub repository subscriptions for event-driven triggers. |
| `stream_jobs` | Long-running streaming task state. |

---

## Managed Processes

Core manages two native processes: `channels` (TypeScript/Bun) and `toolgate` (Python/FastAPI). They are **not** Docker containers — they run as child processes of Core on the host system.

### Channels

**What it does:** Connects to messaging platforms (Telegram, Discord, Matrix, IRC, Slack, WhatsApp). Maintains persistent connections, receives messages, forwards them to Core via an internal WebSocket, and delivers agent responses back.

**Directory:** `channels/`

**Runtime dependencies:**
- Bun 1.x installed on the host
- `bun install` run once to install npm dependencies

**Deploy workflow:**
1. Edit drivers in `channels/src/drivers/` on the development machine
2. Use `make deploy-binary` if deploying a full update, or `rsync` just the changed `.ts` files
3. Restart via API: `POST /api/services/channels/restart`
4. Verify: `curl http://localhost:3100/health`

Since Bun runs TypeScript directly (no compilation step), individual `.ts` files can be edited on the server and a restart is sufficient to pick up changes.

**Registering a Telegram channel:**
```bash
# Store the bot token as a scoped secret
curl -X POST http://localhost:18789/api/secrets \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"name": "BOT_TOKEN", "scope": "MyAgent", "value": "1234:abc..."}'

# Register the channel
curl -X POST http://localhost:18789/api/agents/MyAgent/channels \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"channel_type": "telegram", "display_name": "My Bot", "config": {}}'
```

### Toolgate

**What it does:** Media hub for STT (speech-to-text), Vision (image description), TTS (text-to-speech), and image generation. Provides a provider registry — each capability has a swappable backend selected through the admin UI.

**Directory:** `toolgate/`

**Runtime dependencies:**
- Python 3.11+ and `uv` on the host
- Virtual environment at `toolgate/.venv/`

**Initial setup:**
```bash
cd toolgate
uv venv .venv
uv pip install -r requirements.txt
```

**Deploy workflow:**
1. Edit or add router files in `toolgate/routers/` on the server
2. Validate Python syntax: `python3 -m py_compile toolgate/routers/new_router.py && echo OK`
3. Register in `toolgate/app.py`: add import and `app.include_router(name.router)`
4. Restart via API: `POST /api/services/toolgate/restart`
5. Verify: `curl http://localhost:9011/health`

**Toolgate endpoints:**

| Endpoint | Description |
|---|---|
| `POST /v1/audio/transcriptions` | Speech-to-text (OpenAI-compatible format) |
| `POST /describe-url` | Vision: describe an image from a URL |
| `POST /transcribe-url` | STT from a media URL |
| `POST /v1/audio/speech` | Text-to-speech (OpenAI-compatible format) |
| `GET /health` | Health check with provider status |
| `GET /docs` | Interactive API documentation (Swagger UI) |

Provider configuration is stored in `toolgate/providers.json` and managed through the admin UI at `http://localhost:18789`.

---

## Watchdog (config/watchdog.toml)

The watchdog is a built-in Core subsystem (not a separate binary) that monitors all services and restarts them if they become unhealthy. It runs as a background task inside Core.

### [watchdog]

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. When `false`, no health checks or automatic restarts occur. |
| `interval_secs` | integer | `60` | How often (in seconds) to run health checks. |
| `max_restart_attempts` | integer | `3` | Maximum restart attempts per service within `flap_window_secs`. After this limit, the service is marked as failed and no more restarts are attempted until the window resets. |
| `cooldown_secs` | integer | `300` | Minimum seconds to wait between restart attempts for the same service. |
| `grace_period_secs` | integer | `60` | Seconds to wait after a restart before checking health again. Prevents false failures during startup. |
| `flap_window_secs` | integer | `600` | Window (in seconds) over which restart attempts are counted for flap detection. |
| `flap_threshold` | integer | `3` | If a service restarts more than this many times within `flap_window_secs`, it is considered flapping and watchdog stops restarting it. |

```toml
[watchdog]
enabled = true
interval_secs = 60
max_restart_attempts = 3
cooldown_secs = 300
grace_period_secs = 60
flap_window_secs = 600
flap_threshold = 3
```

### [[checks]]

Each `[[checks]]` entry defines one service to monitor. A check uses either a URL (HTTP GET) or a shell command, but not both.

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | Yes | Unique name for this check. Appears in watchdog logs and `/api/doctor`. |
| `url` | string | No | HTTP URL to GET. HTTP 200 = healthy. Use this for services with a `/health` endpoint. |
| `check_cmd` | string | No | Shell command to run. Exit code 0 = healthy, non-zero = unhealthy. Use when a URL check is not available. |
| `restart_cmd` | string | Yes | Shell command to run when the check fails. Core executes this via `sh -c`. |

```toml
[[checks]]
name = "core"
url = "http://localhost:18789/health"
restart_cmd = "systemctl --user restart hydeclaw-core"

[[checks]]
name = "postgres"
check_cmd = "docker exec docker-postgres-1 pg_isready -U hydeclaw"
restart_cmd = "docker restart docker-postgres-1"

[[checks]]
name = "channels"
url = "http://localhost:3100/health"
restart_cmd = "curl -sf -X POST http://localhost:18789/api/services/channels/restart"

[[checks]]
name = "toolgate"
url = "http://localhost:9011/health"
restart_cmd = "curl -sf -X POST http://localhost:18789/api/services/toolgate/restart"

[[checks]]
name = "memory-worker"
check_cmd = "systemctl --user is-active hydeclaw-memory-worker"
restart_cmd = "systemctl --user restart hydeclaw-memory-worker"

[[checks]]
```

### [resources]

Resource threshold monitoring. Watchdog logs warnings and critical alerts when disk or RAM thresholds are crossed. It does not take automatic action — alerts are logged and sent to agents if configured.

| Field | Type | Description |
|---|---|---|
| `disk_warning_gb` | integer | Log a warning when free disk space drops below this many gigabytes. |
| `disk_critical_gb` | integer | Log a critical alert when free disk space drops below this many gigabytes. |
| `ram_warning_percent` | integer | Log a warning when RAM usage exceeds this percentage. |
| `ram_critical_percent` | integer | Log a critical alert when RAM usage exceeds this percentage. |
| `check_interval_secs` | integer | How often to check resource usage. Independent of `interval_secs`. |

```toml
[resources]
disk_warning_gb = 5
disk_critical_gb = 1
ram_warning_percent = 85
ram_critical_percent = 95
check_interval_secs = 300
```

---

## Makefile Targets

The Makefile simplifies common development and deployment tasks. Set `PI_HOST` to your server's SSH address before running deploy targets.

```bash
# Override the deploy target
PI_HOST=user@your-server make deploy-binary
```

| Target | Description |
|---|---|
| `check` | Run `cargo check --all-targets`. Fast syntax/type check without producing a binary. |
| `test` | Run `cargo test`. Executes all Rust unit and integration tests. |
| `lint` | Run `cargo clippy --all-targets -- -D warnings`. All warnings are errors. |
| `build` | Build a native release binary for the current platform: `cargo build --release`. |
| `build-arm64` | Cross-compile for ARM64 (Raspberry Pi): `cargo zigbuild --release --target aarch64-unknown-linux-gnu`. Requires `cargo-zigbuild` and the `zig` toolchain. |
| `ui` | Build the Next.js web UI: `cd ui && npm run build`. Outputs to `ui/out/`. |
| `deploy-binary` | Build ARM64 binary, upload to `$PI_HOST`, stop the service, replace the binary, restart the service. |
| `deploy-ui` | Build UI and upload the `out/` directory to `$PI_DIR/ui/` on the server via tar over SSH. |
| `deploy-migrations` | Upload the `migrations/` directory to the server via scp. |
| `deploy-docker` | Sync `docker/` to the server via rsync (excluding build artifacts), then run `docker compose up -d --build`. |
| `deploy` | Run all deploy targets in sequence: binary, UI, migrations, docker. Then run `make doctor`. |
| `doctor` | SSH to the server and call `GET /api/doctor`, formatted as JSON. Shows the health of all services. |
| `logs` | Stream Core's systemd journal from the server: `journalctl --user -u hydeclaw-core -f`. |
| `restart` | SSH to the server and restart the Core systemd service. |
| `status` | SSH to the server and show the Core systemd service status. |
| `clean` | Remove Rust build artifacts (`cargo clean`) and the UI build output (`ui/out/`, `ui/.next/`). |

### Configuration

| Variable | Default | Description |
|---|---|---|
| `PI_HOST` | `user@your-server` | SSH target for deploy commands. Override on the command line or set in your shell. |
| `PI_DIR` | `~/hydeclaw` | Directory on the server where HydeClaw is installed. |
| `TARGET` | `aarch64-unknown-linux-gnu` | Rust cross-compilation target triple. |
| `AUTH` | Read from `.auth-token` file | Auth token for `make doctor`. Create `.auth-token` with your `HYDECLAW_AUTH_TOKEN` value. |

### First-Time Setup on a New Server

```bash
# 1. Create the directory structure
ssh user@your-server "mkdir -p ~/hydeclaw/{config/agents,workspace/{tools,skills,agents,mcp},docker,migrations}"

# 2. Deploy everything
PI_HOST=user@your-server make deploy

# 3. Set up the systemd service
scp docker/hydeclaw-core.service user@your-server:~/.config/systemd/user/
ssh user@your-server "systemctl --user daemon-reload && systemctl --user enable hydeclaw-core && systemctl --user start hydeclaw-core"

# 4. Start Docker infrastructure
ssh user@your-server "cd ~/hydeclaw/docker && docker compose up -d"

# 5. Check health
PI_HOST=user@your-server make doctor
```
