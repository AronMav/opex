# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
# Rust
make check              # cargo check --all-targets
make test               # cargo test
make lint               # cargo clippy --all-targets -- -D warnings
cargo test test_name -- --nocapture  # single test

# UI (Next.js)
cd ui && npm run build  # production build
cd ui && npm test       # vitest (one-shot)
cd ui && npm run dev    # dev server (port 3000)

# Channel adapter (Bun)
cd channels && bun test
```

## Cross-compilation & Deploy

```bash
make build-arm64        # cargo zigbuild --target aarch64-unknown-linux-gnu
make deploy             # build + scp binary + restart systemd + deploy UI + migrations
make deploy-binary      # binary only
make deploy-ui          # UI only (builds first)
make doctor             # GET /api/doctor health check on Pi
make logs               # journalctl --user -u hydeclaw-core -f
```

**Why zigbuild:** no OpenSSL anywhere — `reqwest` uses `rustls-tls` only. All crates in Cargo.toml use `rustls-tls` feature flags. Never add OpenSSL dependencies.

Deploy target: set via `PI_HOST` env var (e.g. `PI_HOST=user@192.168.1.100`).

## Release

```bash
./release.sh          # build for host architecture
./release.sh --all    # build for aarch64 + x86_64
```

**Version:** single source of truth is `Cargo.toml` (`[workspace.package] version`). Pass version to `release.sh` as argument — it syncs to `Cargo.toml`, `ui/package.json`, `channels/package.json` before building. Releases are published via git tag: `git tag v0.2.0 && git push origin v0.2.0`.

**Scripts in release archive:**

- `setup.sh` — interactive installer (fresh install)
- `update.sh` — one-command updater (`~/hydeclaw/update.sh hydeclaw-v0.2.0.tar.gz`)
- `uninstall.sh` — complete removal

**Paths on Pi:**

- Binary: `~/hydeclaw/hydeclaw-core-aarch64`
- Watchdog: `~/hydeclaw/hydeclaw-watchdog-aarch64`
- Memory worker: `~/hydeclaw/hydeclaw-memory-worker-aarch64`
- UI static: `~/hydeclaw/ui/out/`
- Config: `~/hydeclaw/config/`
- Workspace: `~/hydeclaw/workspace/`
- Migrations: `~/hydeclaw/migrations/`
- Docker: `~/hydeclaw/docker/`

## Architecture

HydeClaw is a Rust-based AI gateway. The core binary (`crates/hydeclaw-core`) handles everything: HTTP API, agent lifecycle, LLM calls, tool execution, channel bridging, memory, and secrets.

### Agent Engine (`src/agent/`)

Three entry points on `AgentEngine`, all thin adapters in [engine/run.rs](crates/hydeclaw-core/src/agent/engine/run.rs) that construct an `EventSink` and delegate to `pipeline::execute`:

- `handle_sse` — web SSE via `SseSink` (over `EngineEventSender`/flume)
- `handle_with_status` — channel adapters (Telegram/Discord) with typing indicator via `ChannelStatusSink` (two `UnboundedSender` channels)
- `handle_streaming` — plain-chunk text via `ChunkSink`

Unified pipeline lives in [src/agent/pipeline/](crates/hydeclaw-core/src/agent/pipeline/):

- `sink.rs` — `EventSink` trait, `PipelineEvent` (`Stream(StreamEvent)` | `Phase(ProcessingPhase)`), `SinkError`, three production sinks
- `bootstrap.rs` — session entry, user-message persist, WAL `running`, `ProcessingGuard`, slash-command detection
- `execute.rs` — main LLM+tools loop, transport-agnostic
- `finalize.rs` — single exit point: persist assistant or partial, WAL `done|failed|interrupted` via `SessionLifecycleGuard`, enqueue knowledge extraction

**Key execution paths:**
- `pipeline::execute::execute()` — LLM call + tool loop, transport-agnostic
- `pipeline::handlers::*` — tool implementations (workspace_write, workspace_read, etc.)
- `workspace.rs::is_read_only()` — path protection

**Loop detection (`tool_loop.rs`):** Two-phase `LoopDetector` — `check_limits()` (pre-execution, read-only) + `record_execution()` (post-execution, tracks success/failure). Error-aware: 3 consecutive errors on same tool → break. WAL records lifecycle events for diagnostics. LoopDetector resets on each session entry (crash recovery via WAL replay is not yet implemented). See design spec at [docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md](docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md).

**Session-scoped agents (`session_agent_pool.rs` + `engine_agent_tool.rs`):** Unified `agent` tool (run/message/status/kill) replaces old `subagent` + `handoff` tools. Agents are always-alive peers bound to a session via `SessionAgentPool` in `AppState.session_pools`. Each `LiveAgent` holds its own LLM dialog context in memory, receives messages via mpsc channel, and processes them in a background tokio task using `run_subagent()`. Polling-based — no automatic routing or turn loop. Peer-to-peer: any agent in a session can spawn, message, or kill any other.

**Agent config** (TOML at `config/agents/{name}.toml`):
- `base = true` — system agent: can't be renamed/deleted, runs on host (no sandbox), can write to service dirs and tools
- `base = true` — cannot be renamed/deleted via API; SOUL.md + IDENTITY.md are immutable
- Both flags are **never** changed via PUT API — preserved from disk on every update
- Agent rename updates 19 DB tables in a transaction (sessions, messages, usage_log, webhooks, etc.)

### Gateway (`src/gateway/`)

Axum HTTP API on port 18789. **Sub-router pattern:** 27 handler modules each export `pub(crate) fn routes() -> Router<AppState>`; `mod.rs` composes them via `.merge()`. Key handlers:

- `agents.rs` — CRUD for agent configs; sorts base agents first
- `chat.rs` — SSE streaming chat endpoint; converts `StreamEvent` to JSON events; bounded channels (256/512) with backpressure
- `sessions.rs` — session CRUD + fork endpoint (`POST /api/sessions/{id}/fork`) + active-path endpoint
- `services.rs` — managed native processes (channels, toolgate) + Docker container management (MCP, browser-renderer)
- `state.rs: agent_names()` / `agent_summaries()` — return agents sorted base-first then alphabetical

**Rate limiting:** 300 rpm default (configurable via `limits.max_requests_per_minute`). Authenticated requests (valid Bearer token) exempt. Auth lockout: 500 failed attempts → 30s block for requests without Authorization header. Loopback exempt.

### Tools (`src/tools/`)

**System tools:** hardcoded in `engine.rs` (memory, workspace_write, workspace_edit, code_exec, agent, etc.). Tool policy `deny` list applies to ALL tools including core system tools — deny is checked first, before the core tools allowlist.

**YAML tools:** `workspace/tools/*.yaml` — define HTTP API calls with optional response transforms:
- `response_transform: "$.path.to.field"` (JSONPath) — extracts from JSON response
- `auth: { type: bearer_env, key: ENV_VAR }` — field is `type:` NOT `auth_type:` (serde rename)
- `required_base: true` — only available to `base = true` agents
- `channel_action:` — after execution, sends binary result via Telegram (send_photo, send_voice)
- Loaded by `load_yaml_tools(workspace_dir)`, found by `find_yaml_tool(workspace_dir, name)`
- **SSRF protection:** YAML tool execution uses `ssrf_http_client` with DNS-level private IP blocking. Path params are URL-encoded, body templates are JSON-escaped. Binary responses limited to 50MB.
- **Tool name validation:** API handlers enforce `[a-zA-Z0-9_-]` on tool and MCP entry names (prevents path traversal)

**Service registry:** `config/services/*.yaml` — internal service definitions (browser-renderer, toolgate, STT, TTS, embedding, vision). These are infrastructure entries (URL, healthcheck, concurrency), NOT agent tools. Loaded by `service_registry.rs`.

**Agent skills:** `workspace/skills/*.md` — shared skills for all agents. `config/skills/*.md` — system skills only available to base agents (provider-management, channel-management, etc.). Loaded by `skills/mod.rs`.

**Agent scaffold:** `crates/hydeclaw-core/scaffold/base/` and `scaffold/regular/` — template SOUL.md, IDENTITY.md, HEARTBEAT.md created for new agents. Base agent gets full system template (capabilities, security rules, API reference). Regular agents get lighter template that delegates system tasks to base. Templates use `{AGENT_NAME}` placeholder.

**MCP tools:** external MCP servers run as Docker containers (on-demand via bollard).

### Channels (`src/channels/` + `channels/` TypeScript)

In-process channel adapter: `InProcessChannelManager` manages channel lifecycle. TypeScript code in `channels/` runs as a managed child process (NOT Docker). Communication via internal WebSocket loopback.

Channel credentials (`bot_token`, `access_token`, `password`, `app_token`) are extracted from the config on create/update and stored in the encrypted vault under key `CHANNEL_CREDENTIALS`, scope = channel UUID string. The JSONB `config` column in `agent_channels` never contains credential values — they are redacted before DB insert and re-injected from vault on `GET ?reveal=true`.

Agent opts in via TOML: `[agent.channel.telegram] enabled = true`

### Memory (`src/memory.rs`)

PostgreSQL pgvector. Hybrid search: semantic (halfvec) + FTS. MMR reranking. Two tiers: raw (time-decay) + pinned permanent. Embedding is delegated to Toolgate (`POST /v1/embeddings`), which proxies to the configured embedding backend via the `providers` table. Core never calls Ollama or any embedding service directly. Config: `[memory]` section in `hydeclaw.toml` — no `embed_url`/`embed_model` keys (those are managed through the providers registry). `embed_dim` is auto-detected at startup.

**Text normalization SoT:** TTS-specific normalization (numbers→words, English→Cyrillic transliteration) lives in `toolgate/normalize.py` and is **NOT** reused for indexing — it is destructive for embedding/search by design.

**`MEMORY.md` vs `memory_chunks`:** complementary, not redundant.

- `workspace/agents/{Agent}/MEMORY.md` (and other workspace `.md`/`.txt` files) — **hand-edited agent state**, the canonical source of truth. Lives in git-friendly text files. Agents read it on every session start.
- `memory_chunks` (PostgreSQL + pgvector) — **searchable index** of the same content plus runtime knowledge (session summaries, extracted facts). Powers hybrid semantic+FTS search.
- Sync is one-way (file → DB) and event-driven: [memory/watcher.rs](crates/hydeclaw-core/src/memory/watcher.rs) listens for workspace file `Create`/`Modify` events and re-indexes the changed file as `scope='shared'`. Editing `MEMORY.md` updates `memory_chunks`; editing `memory_chunks` directly does NOT update `MEMORY.md`.
- **First-run bootstrap:** the watcher is delta-only (no initial scan). On startup, if `memory_chunks` has zero `scope='shared'` rows, [main.rs](crates/hydeclaw-core/src/main.rs) enqueues a one-shot reindex task (after a toolgate-readiness probe) so workspace files get indexed without manual intervention. Subsequent restarts skip the bootstrap. Operator can re-trigger anytime via the `memory.reindex` agent action.

### Secrets (`src/secrets.rs`)

ChaCha20Poly1305 encryption, stored in `secrets` table. Resolution order: `(name, scope)` → `(name, "")` global → env var.

### Notifications (`src/gateway/handlers/notifications.rs` + `src/db/notifications.rs`)

PostgreSQL-backed notification system with real-time WebSocket broadcast. Notifications created via `notify(db, ui_event_tx, type, title, body, data)` — persists to DB + broadcasts to all WS clients.

**Triggers:** `access_request` (pairing code), `tool_approval` (agent needs approval), `agent_error` (run failed), `watchdog_alert` (service down).

**API:** `GET /api/notifications` (list + unread_count), `PATCH /api/notifications/{id}` (mark read), `POST /api/notifications/read-all`, `DELETE /api/notifications/clear`.

**UI:** Bell icon in sidebar footer with badge counter, dropdown list, click navigates to relevant page, sound on new notification.

**Note:** Backend serializes `notification_type` field as `"type"` in JSON (serde rename). Frontend `NotificationRow.type` matches this.

### Network Discovery (`src/gateway/handlers/network.rs`)

`GET /api/network/addresses` returns WAN IP (with CGNAT detection), Tailscale status, LAN interfaces, mDNS hostname (`hydeclaw.local`). WAN IP cached for 5 minutes. mDNS registered at startup via `mdns-sd` crate.

### Setup Wizard

4-step wizard: requirements check → provider + API test → agent creation → Telegram channel.

- `GET /api/setup/status` — `{needs_setup: bool}` from `system_flags` table (not agent count)
- `GET /api/setup/requirements` — Docker, PostgreSQL, disk space checks (pre-auth, no token needed)
- `POST /api/setup/complete` — guarded by `setup_guard_middleware` (403 after completion)
- First agent auto-gets `base = true`, `access.mode = "restricted"`, tool deny-list
- Non-base agents auto-get deny-list: `code_exec`, `process_start`, `workspace_delete`, `workspace_rename`

### SSE Streaming

Chat endpoint streams `StreamEvent` variants over SSE (Vercel AI SDK v3 compatible). Event types are defined in `gateway/mod.rs` (`sse_types` module) and mirrored in `ui/src/stores/sse-events.ts`:

| Event type | Direction | Payload |
| --- | --- | --- |
| `"data-session-id"` | first | `{sessionId}` |
| `"start"` | open | `messageId?` |
| `"text-start"` | text block open | `id?` |
| `"text-delta"` | streaming text | `delta` |
| `"text-end"` | text block close | — |
| `"tool-input-start"` | tool call begins | `toolCallId`, `toolName` |
| `"tool-input-delta"` | tool input streaming | `toolCallId`, `inputTextDelta` |
| `"tool-input-available"` | full tool input ready | `toolCallId`, `input` |
| `"tool-output-available"` | tool result ready | `toolCallId`, `output` |
| `"file"` | inline media | `url`, `mediaType?` |
| `"rich-card"` | structured card | `cardType`, `data` |
| `"sync"` | message sync | `content`, `toolCalls`, `status`, `error?` |
| `"finish"` | stream end | — |
| `"error"` | stream error | `errorText` |

File events: tool handlers emit `FILE_PREFIX = "__file__:"` inline in tool result; `save_binary_to_uploads()` saves to `workspace/uploads/` and returns `/uploads/{uuid}.ext` URL. The `/uploads/*` path is excluded from auth middleware.

## Configuration

**Main config:** `config/hydeclaw.toml` — server, DB, embedding, managed processes.

**Agent config:** `config/agents/{Name}.toml` — case-sensitive filename matches agent name.

**Environment:** `.env` in binary dir (auto-loaded). **Policy: only 3 keys belong in `.env`:**

- `HYDECLAW_AUTH_TOKEN` — HTTP API auth token
- `HYDECLAW_MASTER_KEY` — vault encryption key
- `DATABASE_URL` — PostgreSQL connection string

All other configuration (service URLs, API keys, tokens) must go into the secrets vault or `config/hydeclaw.toml`. Never add extra keys to `.env`.

## Frontend (`ui/`)

Next.js 16 App Router, React 19, Tailwind 4, shadcn/ui, Zustand state, CodeMirror editor.

Post-build script `scripts/flatten-rsc.mjs` flattens RSC chunks — required for static nginx serving. Run as part of `npm run build`.

**Key stores:** `chat-store.ts` (451 lines — core state machine + actions), `auth-store.ts` (health check + agent list), `canvas-store.ts` (workspace canvas state).

**Chat store decomposition** (Phase 54): `chat-store.ts` was 1891 lines, now split into:

- `chat-types.ts` — types (ChatMessage, MessagePart, AgentState, ConnectionPhase, MessageSource)
- `chat-history.ts` — convertHistory, resolveActivePath, findSiblings, getCachedRawMessages
- `chat-reconciliation.ts` — contentHash, reconcileLiveWithHistory
- `chat-persistence.ts` — saveLastSession, getLastSessionId, getInitialAgent (localStorage)
- `streaming-renderer.ts` — factory via `createStreamingRenderer()`: SSE parsing, rAF throttling (50ms), reconnection, per-agent Map cleanup. Non-serializable state (AbortController, setTimeout) in private closures, not Immer

**Chat components** (`ui/src/components/chat/`): ApprovalCard, ApprovalCountdown, ApprovalArgsEditor, ContinuationSeparator, HandoffDivider, ReconnectingIndicator, StepGroup, ToolCallPartView

**Utilities:** `card-registry.tsx` (CARD_REGISTRY + GenerativeUISlot + CardErrorBoundary), `citation-tooltip.tsx` (footnote tooltips), `tool-state.ts` (ToolPartState mapper), `use-smoothed-text.ts` (adaptive text streaming animation)

**API types:** `ui/src/types/api.ts` — keep `AgentInfo`, `WebhookEntry`, `ApprovalEntry` etc. in sync with backend JSON responses. SSE event types in `ui/src/stores/sse-events.ts`.

## Database

PostgreSQL 17 + pgvector. Migrations in `migrations/` (sqlx). Auto-run on startup. No ORM — raw sqlx queries in `src/db/`.

Key tables: `sessions`, `messages`, `session_events` (WAL journal), `memory_chunks`, `scheduled_jobs`, `secrets`, `agent_channels`, `usage_log`, `providers`, `provider_active`, `watchdog_settings`.

**Message branching (m012):** `parent_message_id` links to predecessor, `branch_from_message_id` marks fork points. Both nullable — NULL = trunk. Enables conversation tree navigation.

**Session WAL (m013):** `session_events` logs lifecycle transitions (running, tool_start, tool_end, done, failed). WAL records lifecycle events for diagnostics. LoopDetector resets on each session entry (crash recovery via WAL replay is not yet implemented).

**Active providers:** `provider_active` maps capabilities (stt, tts, vision, imagegen, embedding) to providers. UI configures active providers via the Active Providers page.

## Process Manager

`src/process_manager/` manages native child processes. **Both `channels` and `toolgate` are native processes, NOT Docker containers.** They run as managed subprocesses spawned by Core at startup, with automatic restart on crash.

- **channels** (`channels/` — TypeScript/Bun): Telegram/Discord/Matrix/IRC/Slack adapters. Started by Core, communicates via internal WebSocket loopback.
- **toolgate** (`toolgate/` — Python/FastAPI): Media hub (STT, Vision, TTS, ImageGen, Embeddings). Started by Core with `--workers 1 --loop asyncio` (single process, no multiprocessing workers). Key endpoints: `POST /describe-url`, `POST /transcribe-url`, `POST /v1/audio/speech`, `POST /v1/embeddings`.

Config in `config/hydeclaw.toml` under `[[managed_process]]`. Restart via `POST /api/services/{name}/restart`. Container restart API has a whitelist — only non-sensitive containers (browser-renderer, searxng, mcp-*) can be restarted; postgres is excluded.

On Pi: `toolgate` source is at `~/hydeclaw/toolgate/` (NOT Docker). To deploy toolgate changes: `scp` changed `.py` files to Pi + `POST /api/services/toolgate/restart`. No Docker build needed.

TTS voices: Qwen3-TTS server natively accepts OpenAI voice names (nova, alloy, echo, fable, onyx, shimmer) — no alias mapping in toolgate. Available voices: `GET /v1/audio/voices` on the TTS server. Default voice: `clone:Arty`. TTS server URL is configured via the providers registry.

## Memory Worker (`crates/hydeclaw-memory-worker/`)

Separate binary (`hydeclaw-memory-worker`) that handles heavy memory tasks asynchronously via a PostgreSQL task queue. Core enqueues tasks; the worker polls and processes them independently.

- Runs as a separate process (own systemd unit or launched alongside core)
- Config: `[memory_worker]` section in `hydeclaw.toml` (`enabled`, `poll_interval_secs`)
- Uses `toolgate_url` to call `POST /v1/embeddings` for reindex tasks
- Recovers stuck `'processing'` tasks on startup (crash safety)
- Task types: `reindex` (rebuild embeddings for workspace files)
- Sends `sd_notify` watchdog pings on Linux

## Watchdog Alerting

Watchdog monitors agent inactivity and managed process health. Alert configuration is DB-backed via the `watchdog_settings` table (not in `hydeclaw.toml`).

- `GET /api/watchdog/settings` — read current alert settings
- `PUT /api/watchdog/settings` — update settings; allowed keys: `alert_channel_ids`, `alert_events`
- `GET /api/watchdog/status` — current watchdog state per agent
- `GET /api/watchdog/config` / `PUT /api/watchdog/config` — per-agent watchdog config
- Alerts are sent via `POST /api/channels/notify` (body: `{"channel_id": "uuid", "text": "..."}`) — used internally by watchdog and available externally

## Graceful Shutdown

On SIGTERM/SIGINT: drains all running agents (calls `handle.shutdown()` on each), then stops managed processes via `process_manager.stop_all()` (sends SIGTERM to process groups, waits 5s, SIGKILL). Graph worker resets stale 'processing' items on next startup.

<!-- GSD:project-start source:PROJECT.md -->
## Project

**HydeClaw Stability Audit**

HydeClaw — Rust-based AI gateway (аналог OpenClaw с более безопасной архитектурой). Единый бинарник обрабатывает HTTP API, жизненный цикл агентов, LLM-вызовы, инструменты, каналы, память и секреты. Проект уже функционирует, цель текущей работы — превентивный аудит и исправление найденных проблем.

**Core Value:** Стабильность и безопасность: найти и устранить баги, несостыковки API, уязвимости и мёртвый код до того, как они проявятся в продакшене.

### Constraints

- **Tech stack**: Rust + rustls-tls only, никакого OpenSSL
- **Deploy target**: ARM64 (Raspberry Pi), single binary
- **Backward compat**: исправления не должны ломать API контракты или миграции
<!-- GSD:project-end -->

<!-- GSD:stack-start source:codebase/STACK.md -->
## Technology Stack

## Languages
- Rust 2024 edition - Core application (`crates/hydeclaw-core`), type definitions, watchdog, memory worker
- TypeScript/Bun - Channel adapters and protocol drivers (`channels/`)
- Python 3 - Media hub and tool gateway (`toolgate/`)
- TypeScript/React - Web UI (`ui/`)
- JavaScript - Build scripts, deployment automation (`scripts/`, `release.sh`)
- SQL - Database migrations and raw queries via sqlx (`migrations/`)
## Runtime
- Rust: tokio async runtime with `full` feature set (signals, time, sync, process, io, macros)
- Node.js/Bun: TypeScript runtime for channel adapters (grammy, discord.js, slack, matrix)
- Python 3.x: FastAPI for toolgate (uvicorn with single-process asyncio loop)
- Cargo - Rust dependencies, workspace members management
- npm/pnpm - UI and channel dependencies
- pip - Python dependencies (FastAPI, httpx, pymupdf, etc.)
- Bun - Channel adapter runtime
## Frameworks
- Axum 0.8 - HTTP API framework with WebSocket support
- Tower 0.5 - HTTP middleware and tower-http with CORS, tracing, FS serving
- sqlx 0.8 - Async PostgreSQL driver (raw SQL, no ORM; rustls-tls only)
- tokio - Async runtime for agents, processes, IO
- reqwest 0.12 - HTTP client for LLM providers and webhooks (rustls-tls, no OpenSSL)
- grammy 1.30.0 - Telegram bot framework
- discord.js 14.16.0 - Discord client
- @slack/bolt 4.1.0 - Slack Bolt framework
- matrix-bot-sdk 0.7.0 - Matrix client
- irc-framework 4.13.0 - IRC client
- FastAPI - REST API framework for media processing
- uvicorn - ASGI server (single process, --workers 1 --loop asyncio)
- httpx - Async HTTP client (120s timeout)
- Next.js 16.1.7 - App Router, React Server Components
- React 19.2.4 - UI library
- Tailwind CSS 4 - Utility-first styling
- shadcn/ui - Component library
- TipTap 3.20.4 - Rich text editor
- CodeMirror 4.25.8 - Code editor
- React Query 5.91.0 - Data fetching and caching
- Zustand 5.0.12 - State management
- Vercel AI SDK v3 - SSE parsing and chat state
- Marked 17.0.4, react-markdown 10.1.0 - Markdown rendering
- Mermaid 11.13.0 - Diagram rendering
- KaTeX 0.16.38 - Math rendering
- vitest 4.1.0 - Unit/component tests
- @testing-library/react 16.3.2 - Component testing utilities
- serde/serde_json 1 - JSON serialization (everywhere: Rust, JS, Python)
- toml 0.8 - TOML parsing for config files
- base64 0.22 - Encoding
- uuid 1 - UUID generation and parsing
## Key Dependencies
- PostgreSQL 17 + pgvector extension - Primary database with vector search
- sqlx 0.8 with `migrate` feature - Auto-run migrations on startup
- sd-notify 0.4 - Systemd watchdog integration (Linux only)
- tokio-cron-scheduler 0.13 + cron 0.13 - Agent heartbeats and scheduled jobs
- chacha20poly1305 0.10 - ChaCha20-Poly1305 vault encryption (pure Rust, no OpenSSL)
- hmac 0.12, sha2 0.10 - HMAC signatures and hashing
- hex 0.4, rand 0.9 - Encoding and randomness for key generation
- subtle 2 - Constant-time comparison
- tracing 0.1, tracing-subscriber 0.3 - Structured logging with JSON output
- opentelemetry 0.27, opentelemetry-otlp 0.27 (optional, behind `otel` feature) - OTLP tracing
- tracing-opentelemetry 0.28 - Bridge to OpenTelemetry
- bollard 0.18 - Docker API for MCP container management
- scraper 0.22 - HTML parsing for link understanding
- reqwest 0.12 with rustls-tls - All external API calls (LLMs, Toolgate, webhooks)
- notify 7 - Config file hot-reload
- tokio-stream 0.1, tokio-util 0.7 - Stream utilities
- futures-util 0.3, futures-core 0.3 - Futures combinators
- async-trait 0.1 - Async trait support
- async-stream 0.3 - Stream macros
- chrono 0.4 - Datetime with serde support
- uuid 1 - UUID v4 generation
- anyhow 1 - Flexible error handling with context
- thiserror 2 - Error macros and derives
- regex 1 - Pattern matching for error classification
## Configuration
- `.env` file (auto-generated on first run if missing)
- Only 3 keys required: `HYDECLAW_AUTH_TOKEN`, `HYDECLAW_MASTER_KEY`, `DATABASE_URL`
- Auto-loaded from binary directory or current working directory
- In production: via systemd `EnvironmentFile=`
- `config/hydeclaw.toml` - Server, database, limits, Docker, memory, managed processes
- `config/agents/{Name}.toml` - Individual agent configuration (case-sensitive filename)
- `Makefile` - Cross-compilation targets (`make check`, `make test`, `make build-arm64`)
- `release.sh` - Multi-architecture release build (aarch64 + x86_64)
- `Cargo.toml` workspace with 4 crates
- `Cargo.lock` - Locked dependency versions
- `docker/docker-compose.yml` - Infrastructure (Postgres, searxng, browser-renderer, MCP servers)
- `docker/Dockerfile.*` - Custom images for Postgres (age, pgvector), sandbox (code execution)
- `ui/package.json` with Next.js build + RSC flattening via `scripts/flatten-rsc.mjs`
- `tailwind.config.ts`, `tsconfig.json`, `next.config.js`
- Post-build: RSC chunks flattened for static nginx serving
- `toolgate/config.py` - Provider registry load from Core API at startup
- `toolgate/providers/` - Pluggable STT, TTS, Vision, ImageGen, Embedding providers
- Runtime configuration loaded from `POST /api/providers` on Core startup
- Single source of truth: `Cargo.toml` version; `release.sh` syncs to `ui/package.json`, `channels/package.json`
## Platform Requirements
- Rust 2024 edition with cargo
- Node.js/Bun for channel adapters
- Python 3.x for toolgate
- Docker (for compose, sandbox, MCP containers)
- PostgreSQL 17 with pgvector extension
- Make (or manual commands from Makefile)
- Binary deployment: `hydeclaw-core`, `hydeclaw-watchdog`, `hydeclaw-memory-worker`
- Systemd service units (on Linux)
- PostgreSQL 17 + pgvector (managed via docker-compose or external)
- Bun runtime for channel adapters (native process, not Docker)
- Python 3.x runtime for toolgate (native process, not Docker)
- Docker daemon (for MCP containers on-demand, browser-renderer, searxng)
- nginx (for static UI serving post-build)
- Linux aarch64 (Raspberry Pi, ARM servers) - via `make build-arm64` (zigbuild)
- Linux x86_64
- macOS (Intel/Apple Silicon) - cross-compilation untested
- Windows (WSL2 required)
<!-- GSD:stack-end -->

<!-- GSD:conventions-start source:CONVENTIONS.md -->
## Conventions

## Naming Patterns
- React components: PascalCase (e.g., `AgentEditDialog.tsx`, `LanguageToggle.tsx`)
- Utilities and hooks: camelCase (e.g., `api.ts`, `queries.ts`, `chat-store.ts`)
- Rust modules: snake_case (e.g., `engine.rs`, `json_repair.rs`, `cli_backend.rs`)
- TypeScript stores: camelCase with `-store` suffix (e.g., `chat-store.ts`, `auth-store.ts`, `language-store.ts`)
- Rust: snake_case for all functions (e.g., `parse_json_valid_result`, `strip_markdown_fences`, `repair_json`)
- TypeScript: camelCase for all functions (e.g., `buildEnvConfig`, `wsToHttp`, `getToken`, `handleUnauthorized`)
- React hooks: `use` prefix in camelCase (e.g., `useAgents`, `useSecrets`, `useCronJobs`)
- Internal/private TypeScript functions: camelCase, may use leading underscore (e.g., `_resetRedirecting`)
- Rust constants: SCREAMING_SNAKE_CASE (e.g., `REQUEST_TIMEOUT` in Rust, `SESSIONS_PAGE_SIZE` in TypeScript)
- TypeScript constants: SCREAMING_SNAKE_CASE (e.g., `REQUEST_TIMEOUT = 30_000`, `MAX_INPUT_LENGTH = 32_000`)
- Local variables: camelCase in both languages
- Rust structs: PascalCase (e.g., `ProcessingPhase`, `StreamEvent`, `AgentEngine`)
- Rust enums: PascalCase variants (e.g., `MessageRole::System`, `TaskStatus::Running`)
- TypeScript interfaces: PascalCase (e.g., `EnvConfig`, `AgentInfo`, `ChannelActionDto`)
- TypeScript type unions: PascalCase (e.g., `ChannelInbound`, `ChannelOutbound`)
- TypeScript const objects with `as const`: lowercase with underscores where used for union types (e.g., `PHASES = { THINKING: "thinking", CALLING_TOOL: "calling_tool", ... }`)
## Code Style
- TypeScript/JavaScript: No explicit formatter specified beyond ESLint (eslint-config-next is extended)
- Rust: Standard cargo fmt (no custom settings in repository)
- TypeScript: ESLint with Next.js config (`eslint.config.mjs` at `ui/`)
- Rules customized: `react-hooks/set-state-in-effect` and `react-hooks/purity` disabled
- Rust: No explicit clippy configuration; follows standard Rust linting
- Group imports by category
- TypeScript: Comments mark sections with `// ── Section Name ──────`
- Rust: Comments mark sections with `// ── Section name ──────────────────────────────`
## Import Organization
- `@/` points to `ui/src/` (configured in `vitest.config.ts` and tsconfig)
- Used consistently throughout frontend code
## Error Handling
- Use `anyhow::Result<T>` as return type for recoverable errors
- Use `anyhow::bail!()` for returning errors with context (e.g., `anyhow::bail!("approval {} not found", id)`)
- Use `Result` or `anyhow::Result` interchangeably in function signatures
- Errors always include context message (e.g., `"session too short to compact ({} messages)"`)
- Throw `Error` objects with descriptive messages (no custom error classes observed)
- Use `try`/`catch` for async operations that may fail
- Log errors but don't necessarily re-throw (e.g., in `extractError` function)
## Logging
- Frontend: Use `toast` notifications from `sonner` for user-facing errors (e.g., `toast.error("message")`)
- Rust: Uses `tracing` crate (see `Cargo.toml`) but not observed in code samples
- No `console.log` calls in production code observed
## Comments
- Doc comments (`///` in Rust, `/** */` in TypeScript/JSDoc) for public functions
- Inline comments (`//`) for non-obvious logic or complex algorithms
- Section headers with visual separators (`// ── Section Name ──────`)
- Used selectively in Rust for public APIs
- Not heavily used in TypeScript (types are preferred for documentation)
## Function Design
- `engine.rs` is 127KB with many `pub async fn` methods
- Smaller utility functions tend to be under 50 lines
- Use named parameters; avoid positional arguments when more than 3
- Async functions take `&self` for methods or explicit parameters
- TypeScript functions use object parameters for multiple values (e.g., `recordItem({ agentId: string, userId: string })`)
- Rust: Always explicit `-> Result<T>` or `-> T` in signatures
- TypeScript: Often implicit through inference; generics used for data (e.g., `useQuery<T>(...): UseQueryResult<T>`)
- Custom return types for complex results (e.g., `(facts_extracted, new_message_count)` tuple from `compact_session`)
## Module Design
- Rust: Use `pub fn` or `pub async fn` for public APIs; private functions are unprefixed
- TypeScript: Named exports are standard (e.g., `export function useAgents() { ... }`)
- Barrel files pattern used: `ui/src/components/ui/` directory likely re-exports components (confirmed via button imports)
- Not explicitly observed but structure suggests they exist for component organization
- `@/components/ui/` path includes multiple UI imports (button, dialog, dropdown, etc.)
## Serialization and Data Transfer
- Rust: Uses `serde` with `#[serde(...)]` attributes (e.g., `#[serde(rename_all = "lowercase")]`, `#[serde(skip_serializing_if = "Option::is_none")]`)
- TypeScript: Plain interfaces for API types; no runtime validation framework observed (relies on type system)
- Query keys: Stable tuples used as cache invalidation strategy (e.g., `["agents"]`, `["agents", name]`, `["cron", jobId, "runs"]`)
## State Management
- Use Zustand stores with middleware: `immer` for immutable updates, `devtools` for debugging
- Store structure: `create()` with methods for mutations
- Global stores: `chat-store.ts`, `auth-store.ts`, `language-store.ts`, `canvas-store.ts`
## Type Definitions
- Derive macros used consistently: `#[derive(Debug, Clone, Serialize, Deserialize)]`
- Serde rename attributes for JSON compatibility (e.g., `#[serde(rename_all = "lowercase")]`)
- Interface-first approach for API contracts
- Union types for flexible data (e.g., `StreamEvent` with multiple variants)
- Readonly tuples for query keys (e.g., `["agents"] as const`)
<!-- GSD:conventions-end -->

<!-- GSD:architecture-start source:ARCHITECTURE.md -->
## Architecture

## Pattern Overview
- A single Axum HTTP API (`hydeclaw-core`) handles all requests
- Each agent runs as an independent tokio task in a long-running LLM loop
- Tool execution is sequential with optional semaphore-limited concurrency
- Communication with external services (Telegram, Discord, Ollama, Toolgate) happens through managed child processes and HTTP clients
- Database changes are applied synchronously via sqlx migrations
- Frontend is a Next.js 16 SPA served static after build
- Single binary deployment (no microservices)
- Async-first throughout (tokio runtime)
- Streaming responses (SSE for chat, WebSocket for live logs)
- No ORM (raw sqlx queries in versioned SQL)
- Rustls-only (no OpenSSL, enables ARM64 cross-compilation)
- Configuration is declarative (TOML for agents + system, YAML for tools)
## Layers
- Purpose: Accept incoming requests, route to agent engines, stream responses back to clients
- Location: `crates/hydeclaw-core/src/gateway/`
- Contains: Axum router, middleware (auth, CORS, rate limit), SSE event marshalling, handler dispatch
- Depends on: Agent engine, database, config, memory, secrets
- Used by: All external clients (web UI, OpenAI-compatible APIs, channel adapters, webhooks)
- Purpose: Main request handler - calls LLM, parses tool calls, executes tools, streams results
- Location: `crates/hydeclaw-core/src/agent/`
- Contains: `engine.rs` (~127KB main loop), provider implementations (OpenAI, Anthropic, Google, HTTP), tool execution, workspace reading, memory augmentation
- Depends on: LLM providers, tools, workspace, memory, secrets, database
- Used by: Gateway handlers (chat, channel, webhooks)
- Purpose: Execute user-requested operations (workspace edit, web search, code execution, custom HTTP calls)
- Location: `crates/hydeclaw-core/src/tools/`
- Contains: YAML tool loader, HTTP client wrappers, SSRF protection, embedding service client
- System tools: hardcoded in `engine.rs` (memory_write, workspace_write, workspace_edit, code_exec, agent, browser_action, etc.)
- YAML tools: loaded from `workspace/tools/*.yaml`, define HTTP API calls with response transforms
- Depends on: Workspace, memory, HTTP client, docker sandbox
- Used by: Agent engine tool execution loop
- Purpose: Store and retrieve contextual information - workspace files, external knowledge, user history
- Location: `crates/hydeclaw-core/src/memory.rs`
- Contains: pgvector queries (semantic + FTS), MMR reranking, embedding delegation to Toolgate
- Depends on: PostgreSQL (pgvector), Toolgate (embeddings proxy)
- Used by: Agent engine (build_context)
- Purpose: Persistent storage - sessions, messages, agents, channels, secrets, usage logs, memory chunks
- Location: `crates/hydeclaw-core/src/db/` (query functions), `migrations/` (schema)
- Contains: sqlx queries (sessions, messages, approvals, audit, usage, providers, etc.), auto-run migrations on startup
- Tables: sessions, messages, memory_chunks, scheduled_jobs, secrets, agent_channels, usage_log, providers, provider_active, webhooks, approvals, audit_log, and 15+ more
- Depends on: None (consumed by all layers)
- Used by: Gateway, engine, memory, scheduler
- Purpose: File-based state - agent configs, YAML tools, workspace files (memory.md, secrets.md, etc.)
- Location: `crates/hydeclaw-core/src/agent/workspace.rs`, `crates/hydeclaw-core/src/config/mod.rs`
- Contains: File I/O (read/write protection), hot-reload monitoring, path validation
- Depends on: Filesystem, notify crate (file watcher)
- Used by: Agent engine, tools, gateway
- Purpose: Spawn and supervise long-running native services (Channels TypeScript, Toolgate Python)
- Location: `crates/hydeclaw-core/src/process_manager/`
- Contains: Process spawning, restart logic, signal handling, stdio capture
- Services: `channels/` (Telegram/Discord/Matrix/IRC/Slack adapters), `toolgate/` (STT/Vision/TTS/ImageGen/Embeddings)
- Depends on: std::process, tokio
- Used by: Main startup routine
- Purpose: Run code in sandbox and MCP servers on-demand
- Location: `crates/hydeclaw-core/src/containers/`
- Contains: bollard Docker client, code sandbox for `code_exec` tool, MCP server launch
- Depends on: Docker daemon, bollard crate
- Used by: Engine (code_exec tool), handlers (MCP startup)
- Purpose: Protect API keys, passwords, credentials (channel bot_token, provider API keys, etc.)
- Location: `crates/hydeclaw-core/src/secrets.rs`
- Contains: ChaCha20Poly1305 encryption, scoped resolution (agent + global), env var fallback
- Depends on: PostgreSQL secrets table, chacha20poly1305 crate
- Used by: Agents (resolve auth in YAML tools), gateway handlers (store/retrieve)
- Purpose: Monitor agent inactivity, managed process health, send alerts via channels
- Location: `crates/hydeclaw-watchdog/src/main.rs` (separate binary)
- Contains: Cron-based monitoring, alert routing to channels
- Depends on: Database, channel router
- Used by: systemd (separate unit)
- Purpose: Handle heavy async memory tasks (embedding reindex) without blocking core
- Location: `crates/hydeclaw-memory-worker/src/main.rs` (separate binary)
- Contains: PostgreSQL task queue polling, embedding calls to Toolgate
- Depends on: Database, Toolgate, tokio
- Used by: systemd (separate unit)
- Purpose: Web UI for managing agents, viewing chat, configuring providers, monitoring
- Location: `ui/src/`
- Contains: App Router pages, Zustand stores, TanStack Query for data fetching, SSE event parser, WebSocket connection
- Depends on: Backend API, Node.js at build time
- Used by: Browser clients (served static after build)
## Data Flow
- Agents: Loaded from `config/agents/{Name}.toml` at startup. In-memory Arc<AgentEngine> per agent. Hot-reload on config file change.
- Sessions: Stored in PostgreSQL. Fetched on-demand (kept in UI memory via Zustand).
- Messages: Stored in PostgreSQL. Streamed during processing, fetched on session load.
- Workspace files: Read on each handle_sse() call (not cached) unless in-process subagent.
- Memory chunks: Stored in pgvector. Queried on build_context (hybrid semantic + FTS search).
- Secrets: Stored encrypted in PostgreSQL. Cached in-memory by SecretsManager (refreshed on set).
- Channel credentials: Stored in secrets vault under `CHANNEL_CREDENTIALS` (not in agent_channels config column).
## Key Abstractions
- Purpose: Abstract LLM backends so engine doesn't care if it's OpenAI, Anthropic, Google, or custom HTTP
- Examples: `crates/hydeclaw-core/src/agent/providers.rs`, `providers_openai.rs`, `providers_anthropic.rs`, `providers_google.rs`
- Pattern: Implement `async fn call_model()` returning token stream. Engine calls repeatedly in tool loop.
- Purpose: Unified representation of all events emitted during message processing (text, tool call, file, error, etc.)
- Examples: `TextDelta("hello")`, `ToolCallStart { id, name }`, `File { url, media_type }`, `Finish { reason }`
- Marshalled to SSE JSON format in `gateway/handlers/chat.rs` for Vercel AI SDK v3 compatibility
- Purpose: Declarative tool definition loaded from YAML - HTTP method, URL template, auth, response transform
- Examples: `workspace/tools/searxng_search.yaml`, `workspace/tools/github_api.yaml`
- Pattern: Load at startup, cache in engine, parse on execution, render auth + body from Jinja2-like templates
- Purpose: Post-tool-call side effect (send_photo, send_voice, send_text to Telegram/Discord)
- Examples: After tool executes, embed result in channel action metadata, route to channel adapter via WebSocket
- Pattern: Prefix tool result with `__file__:` or `__rich_card__:`, engine extracts and routes
- Purpose: Require human approval before executing sensitive tools (e.g. workspace_write, code_exec)
- Pattern: Tool check needs_approval() → create approval record in DB → wait on approval_id waiter (tokio::oneshot) → webhook callback wakes waiter → continue or reject
- Purpose: Track session-scoped agents spawned by `agent` tool, allow status checks and lifecycle management
- Pattern: `SessionAgentPool` per session in `AppState.session_pools`. Each `LiveAgent` runs as a tokio task with cancellation token. Polling-based communication via `agent(action: "status")`.
## Entry Points
- Location: `crates/hydeclaw-core/src/main.rs` (~43KB), `crates/hydeclaw-core/src/gateway/mod.rs`
- Triggers: Startup (`cargo run` or systemd service)
- Responsibilities: Load config, run migrations, spawn agent engines, start process_manager, bind Axum router to port 18789
- Location: `crates/hydeclaw-core/src/agent/engine/run.rs` function `handle_sse()` → `pipeline::execute`
- Triggers: POST `/api/chat`, resumed via `/api/chat/{id}/stream`, webhook tools
- Responsibilities: Build context (bootstrap), call LLM (execute), loop tool execution, stream results (sink), persist session (finalize)
- Location: `crates/hydeclaw-core/src/agent/pipeline/execute.rs` — tool call dispatch
- Triggers: LLM returns tool_calls in response
- Responsibilities: Dispatch by tool type, handle approval workflow, capture result, continue LLM loop
- Location: `crates/hydeclaw-watchdog/src/main.rs`
- Triggers: systemd unit or manual start
- Responsibilities: Poll DB for agent inactivity, send alerts via channel router, restart stale processes
- Location: `crates/hydeclaw-memory-worker/src/main.rs`
- Triggers: systemd unit or manual start
- Responsibilities: Poll task queue (reindex jobs), call Toolgate embeddings, update memory chunks
- Location: `ui/src/app/(authenticated)/chat/page.tsx`
- Triggers: Browser navigation to `/chat?agent=AgentName`
- Responsibilities: Load agent + session, connect to SSE stream, parse events, render chat UI
## Error Handling
- **LLM Call Fails:** Engine catches, retries once, then emits StreamEvent::Error(). Session marked 'failed' in DB.
- **Tool Execution Fails:** Result is "tool error: {message}". Engine appends to context and continues loop (no crash).
- **Approval Timeout:** If approval_id waiter timeout expires, engine continues with rejection.
- **Workspace File Missing:** Engine reads file, returns empty string or error message (based on tool spec).
- **Database Error:** Handler returns 500 StatusCode with error details. No partial state left (transactions used).
- **Docker Timeout:** Code_exec tool returns timeout error. Sandbox cleans up container.
- **Channel Send Failure:** Log warning and continue; don't block agent processing.
## Cross-Cutting Concerns
- Framework: `tracing` crate with `tracing-subscriber` JSON output
- Broadcast layer: Events also sent to connected WebSocket clients (UI logs page) via BroadcastLogLayer
- Example: `tracing::info!(agent = %name, "agent started")`
- Config: TOML parsed with serde; missing required fields cause startup error
- Tool names: API enforces `[a-zA-Z0-9_-]` pattern (prevents path traversal in workspace/tools lookup)
- Workspace paths: `workspace.rs:is_read_only()` prevents tool from writing outside allowed dirs
- SSRF: YAML tool HTTP client uses custom DNS resolver to block private IP ranges (169.254.x.x, 10.x.x.x, 127.x, 172.16-31.x, 192.168.x)
- API Token: Single bearer token (env var HYDECLAW_AUTH_TOKEN) checked by middleware
- Session ID: Opaque UUID, returned first in SSE (data-session-id event)
- WebSocket Ticket: One-time ticket issued by POST `/api/auth/ws-ticket`, consumed by WS connection
- Channel OAuth: Separate OAuth2 flow in `oauth.rs` for Telegram, Discord, etc.
- Per-minute limit: Configured in `limits.config.max_requests_per_minute` (default 100)
- Tool concurrency: Semaphore in engine limits concurrent tool execution (default 10, configurable)
- Request timeout: Global timeout per request (default 180 seconds, configurable)
<!-- GSD:architecture-end -->

<!-- GSD:workflow-start source:GSD defaults -->
## GSD Workflow Enforcement

Before using Edit, Write, or other file-changing tools, start work through a GSD command so planning artifacts and execution context stay in sync.

Use these entry points:
- `/gsd:quick` for small fixes, doc updates, and ad-hoc tasks
- `/gsd:debug` for investigation and bug fixing
- `/gsd:execute-phase` for planned phase work

Do not make direct repo edits outside a GSD workflow unless the user explicitly asks to bypass it.
<!-- GSD:workflow-end -->

<!-- GSD:profile-start -->
## Developer Profile

> Profile not yet configured. Run `/gsd:profile-user` to generate your developer profile.
> This section is managed by `generate-claude-profile` -- do not edit manually.
<!-- GSD:profile-end -->
