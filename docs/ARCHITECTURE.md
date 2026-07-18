# OPEX Architecture

## Overview

OPEX is a self-hosted AI gateway and multi-agent platform written in Rust. Primary deployment target is Raspberry Pi 4 (ARM64). The stack consists of three Rust binaries (`opex-core`, `opex-watchdog`, `opex-memory-worker`) running as independent systemd services, with two managed child processes (`channels` in Bun/TypeScript, `toolgate` in Python/FastAPI) spawned by Core at startup.

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        opex-core (systemd)                              │
│                                                                         │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐  ┌──────────────────┐  │
│  │AgentEngine │  │AgentEngine │  │ Scheduler  │  │   Gateway/Axum   │  │
│  │ "Agent1"   │  │ "Agent2"   │  │ (cron jobs)│  │  :18789          │  │
│  └──────┬─────┘  └──────┬─────┘  └────────────┘  └────────┬─────────┘  │
│         │               │                                  │            │
│  ┌──────▼───────────────▼──────────────────────────────────▼─────────┐  │
│  │   ChannelActionRouter  /  SessionAgentPool  /  StreamRegistry     │  │
│  └────────────────────────────────────────────────────────────────────┘  │
│                                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐  │
│  │ SecretsVault │  │ MemoryStore  │  │ McpRegistry  │  │ToolRegistry│  │
│  │ ChaCha20     │  │ pgvec+FTS    │  │  (bollard)   │  │RwLock<Map> │  │
│  │             │  │ +trigram     │  │              │  │            │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  └────────────┘  │
└──────────────────────────────┬──────────────────────────────────────────┘
                               │  manages child processes
                     ┌─────────┴──────────┐
                     │                    │
            ┌────────▼───────┐   ┌────────▼──────┐
            │  channels      │   │  toolgate      │
            │  (Bun/TS)      │   │  (Python 3)    │
            │  Telegram,     │   │  STT, TTS,     │
            │  Discord, etc. │   │  Vision, Embed │
            │  WS ↔ core     │   │  port 9011     │
            └────────────────┘   └────────────────┘

┌────────────────────┐  ┌──────────────────────┐
│ opex-watchdog      │  │ opex-memory-          │
│ (systemd)          │  │ worker (systemd)      │
│ health monitor     │  │ LISTEN/NOTIFY + poll  │
│ alert routing      │  │ embedding reindex     │
└─────────┬──────────┘  └────────────┬──────────┘
          │                          │
          └──────────────┬───────────┘
                         │
          ┌──────────────▼──────────────────┐
          │   PostgreSQL 17 + pgvector       │
          │   + pg_trgm                      │
          │   sessions, messages,            │
          │   memory_chunks, secrets,        │
          │   memory_tasks, agent_channels   │
          └─────────────────────────────────┘
```

**Key binaries:**

| Binary | Role |
|--------|------|
| `opex-core` | Main process: HTTP gateway, agent engines, scheduler, secrets, MCP, channels WS |
| `opex-memory-worker` | Background indexing via PostgreSQL LISTEN/NOTIFY + poll safety net |
| `opex-watchdog` | External health monitor — agent inactivity and managed process health alerts |

**Stack:** axum 0.8, tokio (4 worker threads), sqlx 0.8 (async PostgreSQL), bollard 0.18 (Docker), reqwest 0.12 (rustls-tls only, no OpenSSL). Binary size ~14 MB, idle RAM ~2.2 MB.

---

## Binaries & Processes

### opex-core

Entry point: `crates/opex-core/src/main.rs`.

Startup sequence:
1. Load `.env` from binary dir (auto-generate if missing with `OPEX_AUTH_TOKEN`, `OPEX_MASTER_KEY`, `DATABASE_URL`)
2. Load `config/opex.toml`
3. Run sqlx migrations (`migrations/*.sql`) automatically
4. Stale `session_timeline` entries from a previous crash are not replayed; LoopDetector is warmed from tool_end events only.
5. Bootstrap `SecretsManager` (decrypt `secrets` table into in-memory cache)
6. Detect `embed_dim` from Toolgate (auto-probe at startup)
7. Load agents from `config/agents/{Name}.toml`, build `Arc<AgentEngine>` per agent
8. If `memory_chunks` has zero `scope='shared'` rows → enqueue one-shot reindex task
9. Start `Scheduler` (tokio-cron-scheduler): heartbeats, dynamic cron, memory decay, backup, curator
10. Start `ProcessManager`: spawn `channels` (Bun) and `toolgate` (Python) as managed child processes
11. Start `ChannelActionRouter`, `SessionAgentPool` map, `StreamRegistry`
12. Bind Axum router on `0.0.0.0:18789`

### opex-memory-worker

Entry point: `crates/opex-memory-worker/src/main.rs`.

- Runs as `tokio::main(flavor = "current_thread")` (single-threaded async)
- DB pool: 3 connections
- Wake strategy: PostgreSQL `LISTEN memory_tasks_new` (primary, sub-100ms pickup) + poll every `poll_interval_secs` (safety net for dropped notifications)
- On startup: recovers stuck `'processing'` tasks from previous crash
- Sends `sd_notify(READY=1)` on Linux for systemd watchdog integration
- Task type: `reindex` — reads workspace files, calls `POST /v1/embeddings` on Toolgate, inserts/updates `memory_chunks`

### opex-watchdog

Entry point: `crates/opex-watchdog/src/main.rs`.

- Monitors agent inactivity and managed process health
- Alert configuration stored in `watchdog_settings` DB table (not `config/opex.toml`)
- Sends alerts via `POST /api/channels/notify` (body: `{"channel_id": "uuid", "text": "..."}`)
- API: `GET/PUT /api/watchdog/settings`, `GET /api/watchdog/status`, `GET/PUT /api/watchdog/config`

### channels (Bun/TypeScript)

Entry point: `channels/src/index.ts`.

- Runs as a managed child process spawned by Core's `ProcessManager`
- Polls `GET /api/channels?reveal=true` every 10 seconds for active channel configs
- Supports 7 platforms via driver factories: **Telegram** (grammy), **Discord** (discord.js), **Matrix** (matrix-bot-sdk), **IRC** (irc-framework), **Slack** (@slack/bolt), **WhatsApp**, **Email**
- Communicates with Core via WebSocket loopback at `/ws/channel/{agent_name}`
- Health server on `HEALTH_PORT` (default 3000)

### toolgate (Python/FastAPI)

Entry point: `toolgate/app.py`.

- Runs as a managed child process (`--workers 1 --loop asyncio`)
- Media hub for STT, Vision, TTS, ImageGen, Embeddings
- Loads provider configuration from Core API at startup via `registry.aload()`
- Key endpoints:
  - `POST /describe-url` — vision (image description)
  - `POST /transcribe-url` — STT (audio transcription)
  - `POST /v1/audio/speech` — TTS
  - `POST /v1/embeddings` — text embeddings (proxied to configured embedding backend)
  - `GET /health` — healthcheck (public, no auth)
  - `POST /reload` — reload provider registry
- httpx connection pool: max 20 connections, max 10 keepalive, pool timeout 120s
- Auth: Bearer token (`AUTH_TOKEN` env var), with internal network bypass (`INTERNAL_NETWORK` CIDR)

---

## Agent Engine

### Architecture After Decomposition (Phase 66)

`AgentEngine` is the central object per agent. It is constructed once at startup, stored in `Arc<AgentEngine>`, and shared across concurrent requests. After Phase 66 decomposition, the monolithic `engine.rs` is split into submodules under `crates/opex-core/src/agent/engine/`:

| File | Role |
|------|------|
| `mod.rs` | `AgentEngine` struct, `Arc<AgentConfig>`, state fields |
| `run.rs` | Three thin entry-point adapters: `handle_sse`, `handle_with_status`, `handle_isolated` |
| `context_builder.rs` | `impl ContextBuilderDeps for AgentEngine`: `build_context`, channel info cache |
| `tool_executor.rs` | `impl ToolExecutor for AgentEngine`: routes single tool call to pipeline |
| `yaml_tool_runner.rs` | YAML tool HTTP execution logic |
| `approval_flow.rs` | Approval gate: `check_needs_approval`, `wait_for_approval` |
| `loop_detector_integration.rs` | `LoopDetector` warm-up from session timeline on session entry |
| `stream.rs` | `ProcessingGuard`, `ProcessingPhase` enum |

`AgentConfig` (`crates/opex-core/src/agent/agent_config.rs`) is an immutable snapshot holding all engine dependencies, grouped into five concern areas:
- **Identity**: `agent: AgentSettings`, `workspace_dir`, `default_timezone`, `app_config`
- **LLM**: `provider: Arc<dyn LlmProvider>`, `compaction_provider`
- **Data**: `db: PgPool`, `memory_store`, `embedder`
- **Tools**: `tools: ToolRegistry`, `approval_manager`
- **Infra**: `scheduler`, `agent_map`, `session_pools`, `audit_queue`, `metrics: Arc<MetricsRegistry>`

### Four Entry Points

All four entry points (`run.rs`) construct an `EventSink` and delegate to `pipeline::{bootstrap, execute, finalize}`:

1. **`handle_sse(msg, event_tx, resume_session_id, force_new_session, cancel)`** — web SSE via `SseSink` wrapping an `EngineEventSender` (bounded `mpsc::Sender<StreamEvent>`, capacity 256). Fires `HookEvent::BeforeMessage` before any processing; publishes `sse_event_tx` so `ApprovalManager` can broadcast tool-approval notifications while the stream is live.

2. **`handle_with_status(msg, text_tx, status_tx)`** — channel adapters (Telegram/Discord) via `ChannelStatusSink`; drives typing indicator via `UnboundedSender` channels.

3. **`handle_isolated(msg)`** — called by Scheduler for cron jobs, heartbeats, and inter-agent `agent` tool calls. Creates a throw-away session (or reuses an existing subagent session). No SSE transport.

### Pipeline: bootstrap → execute → finalize

The unified pipeline lives in `crates/opex-core/src/agent/pipeline/`.

**`sink.rs`** defines the transport abstraction:
```rust
enum PipelineEvent {
    Stream(StreamEvent),    // web SSE events
    Phase(ProcessingPhase), // channel typing indicator phases
}
trait EventSink: Send {
    async fn emit(&mut self, ev: PipelineEvent) -> Result<(), SinkError>;
}
```
Production sinks: `SseSink` (SSE), `ChannelStatusSink` (channel), `ChunkSink` (plain text).

**`bootstrap.rs`** — session entry:
1. Resolve or create session (check `resume_session_id`, handle `force_new_session`)
2. Check `pending_split` compaction state → `maybe_split_session()` creates child session B, marks parent A with `end_reason='compression'`
3. Persist user message to DB, write timeline `running` event (with single retry)
4. Build `ProcessingGuard` (marks session in-flight), `SessionLifecycleGuard`
5. Load `Compressor` state from `sessions.compaction_state` JSONB
6. Detect and handle slash-commands → `command_output` early exit
7. Load session history (`max_history_messages`, default 50), assemble `tools` list
8. Warm up `LoopDetector` from session timeline (tool_end events only)
9. Return `BootstrapOutcome { session_id, messages, tools, loop_detector, compressor, ... }`

**`execute.rs`** — main LLM + tools loop:
```
for iteration in 0..loop_config.effective_max_iterations():
    1. Check CancellationToken → Interrupted("cancel_token") on signal
    2. Optionally run mid-loop compaction (Compressor.should_compress())
    3. LLM call: engine.call_provider() → stream text deltas into sink
    4. ThinkingFilter: strip <think>/<thinking>/<thought>/<antthinking> in-stream
    5. Collect tool_calls from response
    6. If tool_calls.is_empty() → check looks_incomplete() → nudge or break
    7. If tool_calls present:
       a. Hook: BeforeToolCall (sync policy, fire_webhooks async)
       b. execute_tool_calls_partitioned(parallel_batch + sequential)
       c. Persist tool results via detached tokio::spawn (survives SSE disconnect)
       d. LoopDetector.check_limits() + record_execution()
    8. If LoopDetector fires → inject nudge or force break
    → return ExecuteOutcome { status, final_text, thinking_json, messages_len_at_end, final_parent_msg_id }
```

**`finalize.rs`** — single exit point:
1. Persist final assistant message to DB (or partial on interruption)
2. Transition `SessionLifecycleGuard`: timeline `done|failed|interrupted`
3. Record `session_failures` row on `Failed` status
4. Emit `notify_agent_error` / `notify_iteration_limit` UI notifications
5. Enqueue knowledge extraction (background `tokio::spawn`, ≥5 messages)
6. Save updated `compressor` state to `sessions.compaction_state`
7. Emit `StreamEvent::Finish` to sink

### Tool Execution Partitioning

Tool calls from a single LLM turn are split into two groups before execution (`pipeline/parallel.rs`):

**Parallel-safe** (read-only or independently stateful):
```
web_fetch, memory_search, memory_get, workspace_read, workspace_list,
tool_list, skill_list, sessions_list, sessions_history, session_search,
session_context, session_export, canvas, rich_card, agent
```
YAML tools with `parallel: true` and no `channel_action` are also parallel-eligible.

**Sequential** — everything else: write operations, tools requiring approval, channel actions.

Parallel batch runs via `futures_util::future::join_all()`. Both batches subject to a 120-second per-tool timeout. A `safety_timeout_secs` (default 600s) wraps the entire `agent` tool call as a defense-in-depth backstop.

**Context enrichment:** before dispatch, the engine injects a `_context` field into each tool call's arguments containing `session_id`, channel identifier, and the original `IncomingMessage.context`. Prevents LLM from forging channel routing data. `_context` is stripped before audit logging (`clean_tool_params`).

### Session Compression Chains

When the context window approaches capacity:

1. `Compressor.should_compress()` checks: `last_prompt_tokens >= context_limit × threshold`
2. Anti-thrash guard: if `ineffective_count >= anti_thrash_max_skips` → skip (prevents thrashing when model responses are already short)
3. `compact_session()` calls a summarization LLM pass, producing `previous_summary`
4. `record_compression_result()`: if savings ≥ `anti_thrash_min_savings` → reset `ineffective_count`, set `pending_split = true`
5. At next session entry, `bootstrap.maybe_split_session()` detects `pending_split = true`:
   - Creates child session B (`sessions.parent_session_id` → A's UUID)
   - Seeds B with: system message + compressed summary + last N messages
   - Sets `sessions.end_reason = 'compression'` on parent A
   - Continues in child session B
6. `CompressorState` is serialized to `sessions.compaction_state` (JSONB). Child state resets `ineffective_count` and `compression_count` but preserves `previous_summary`.

### Hook System (`crates/opex-core/src/agent/hooks.rs`)

`HookRegistry` intercepts engine events for policy enforcement:

```rust
enum HookEvent {
    BeforeMessage,
    AfterResponse,
    BeforeToolCall { agent: String, tool_name: String },
    AfterToolResult { agent: String, tool_name: String, duration_ms: u64 },
    OnError,
}
enum HookAction { Continue, Block(String) }
```

Built-in hooks: `logging_hook()` (trace all calls), `block_tools_hook(blocked: Vec<String>)` (silent deny).

Outbound webhooks (`WebhookConfig { url, events }`): fire-and-forget HTTP POST dispatched via `tokio::spawn` with 5-second timeout. Errors are logged at `warn` and dropped — they never affect `HookAction`. Configured under `[agent.hooks]` in agent TOML.

### Session Agent Pool (`session_agent_pool.rs`)

Each active session maintains its own `SessionAgentPool` in `AppState.session_pools: SessionPoolsMap` (`Arc<RwLock<HashMap<Uuid, SessionAgentPool>>>`).

`LiveAgent` fields:
- `message_tx: mpsc::Sender<AgentMessage>` — send new messages
- `status: Arc<AtomicU8>` — `STATUS_IDLE(0)` / `STATUS_PROCESSING(1)`
- `last_result: Arc<RwLock<Option<String>>>` — last completed response
- `result_notify: Arc<Notify>` — signaled on each IDLE transition (zero-latency vs polling)
- `task_handle: JoinHandle<()>` — background tokio task running `run_subagent()`
- `cancel: Arc<AtomicBool>` — cancellation flag; set on `Drop`

`agent` tool actions:
- `run` / `ask` — spawn on pool-miss, continue dialog on pool-hit, block until `last_result`
- `status` — read `status + last_result` without blocking
- `kill` — remove from pool (Drop signals cancellation)
- `collect` — wait for idle and return last_result

### LLM Providers

28 provider types defined in `PROVIDER_TYPES` (`providers.rs`). All implement `LlmProvider` trait (`chat()` + `chat_stream()`). API keys resolved from `SecretsManager` on each call (hot-reloadable without restart).

Provider routing: per-agent `[[agent.routing]]` rules evaluated in order. Conditions: `"default"`, `"short"` (<300 chars), `"long"` (>2000 chars), `"with_tools"`, `"financial"`, `"analytical"`, `"code"`, `"fallback"`. Each rule references a named DB provider connection. Fallback provider: `agent.fallback_provider` — switches after `max_consecutive_failures` (default 3) errors from primary; per-run only.

### Error Classification

Errors from LLM providers are classified via `LazyLock`-compiled regex patterns into 8 classes:

| Class | Trigger patterns | Recovery |
|-------|-----------------|----------|
| `Billing` | `402`, "payment required", "insufficient credit" | No retry |
| `AuthPermanent` | `401`/`403` + api key, "unauthorized", "api key invalid" | No retry |
| `ContextOverflow` | "context length", "token limit", "input too long" | Compact context, retry |
| `SessionCorruption` | "tool_use_block", "roles must alternate", "orphan tool" | Reset messages, retry once |
| `RateLimit` | `429`, "too many requests", "tokens per minute" | 60s cooldown |
| `Overloaded` | "overloaded", "high demand", "overloaded_error" | 30s cooldown |
| `TransientHttp` | `500`/`502`/`503`/`504`/`521`–`524`/`529`, "bad gateway" | 15s, up to 3 retries |
| `Unknown` | anything else | 15s cooldown |

---

## Session Lifecycle

```
POST /api/chat (new)
       │
       ▼
bootstrap(): resolve/create session
       │ timeline: "running"
       ▼
execute(): LLM + tools loop
       │   ── tool_calls ──► execute_tool_calls_partitioned()
       │                         ├── timeline: tool_start / tool_end per call
       │                         └── tool results persisted via detached spawn
       ▼
finalize(): persist assistant message
       │ timeline: "done" | "failed" | "interrupted"
       │ session_failures row on Failed
       ▼
background: knowledge extraction (≥5 messages)
            compressor state saved to sessions.compaction_state
```

**Session timeline** (`session_timeline` table, migration m013 + renamed by m049): chronological log of session lifecycle events — `running`, `tool_start`, `tool_end`, `done`, `failed`, `interrupted`. Used for LoopDetector warm-up after restart (preserves loop-break decisions across crashes), diagnostics, audit, and the UI Timeline view. Not a Write-Ahead Log: no replay-based recovery; completed work is preserved by persisted side effects, not event replay. Retention: 7 days by default (`cleanup.session_timeline_retention_days`), cleaned in batches of 5000 rows hourly.

**Message branching** (migration m012): `parent_message_id` links each message to its predecessor; `branch_from_message_id` marks fork points. Both nullable (NULL = trunk). Enables conversation tree navigation.

**Mirror messages** (migration m043): `messages.is_mirror = true` marks messages written by cron delivery. Mirror records do NOT update `sessions.last_message_at` (trigger guards this), keeping DM sessions from floating to the top of the list.

**Compression chains** (migration m041): `sessions.parent_session_id` (UUID, FK to sessions) and `sessions.end_reason` ('compression' or NULL). Index on `parent_session_id WHERE NOT NULL`.

---

## Gateway

### Sub-Router Pattern

`crates/opex-core/src/gateway/mod.rs` composes the router via `.merge()` of 31 handler modules. Each handler module exports `pub(crate) fn routes() -> Router<AppState>`.

**Current handler modules** (`src/gateway/handlers/`):

| Module | Routes |
|--------|--------|
| `chat.rs` | `/health`, `POST /api/chat`, `/v1/chat/completions`, `/v1/models`, `/v1/embeddings` |
| `auth.rs` | `POST /api/auth/ws-ticket` |
| `channel_ws.rs` | `GET /ws`, `GET /ws/channel/{agent_name}` |
| `agents.rs` | `/api/agents/*`, `/api/approvals/*` |
| `sessions.rs` | `/api/sessions/*`, `/api/messages/*` |
| `session_failures.rs` | `/api/sessions/failures`, `/api/sessions/{id}/failures` |
| `monitoring.rs` | `/api/setup/*`, `/api/status`, `/api/stats`, `/api/usage/*`, `/api/doctor`, `/api/health/dashboard`, `/api/audit/*`, `/api/watchdog/*` |
| `providers.rs` | `/api/providers/*`, `/api/provider-types`, `/api/media-drivers`, `/api/media-config`, `/api/provider-active` |
| `network.rs` | `GET /api/network/addresses` |
| `secrets.rs` | `/api/secrets/*` |
| `memory.rs` | `/api/memory/*` |
| `cron.rs` | `/api/cron/*` |
| `tools.rs` | `/api/tool-definitions`, `/api/tools/*`, `/api/mcp/*` |
| `yaml_tools.rs` | `/api/yaml-tools/*`, `/api/agents/*/yaml-tools/*` |
| `skills.rs` | `/api/skills/*`, `/api/agents/*/skills/*` |
| `channels.rs` | `/api/channels/*`, `/api/agents/*/channels/*`, `/api/agents/*/hooks` |
| `config.rs` | `/api/config/*`, `/api/restart`, `/api/tts/*`, `/api/canvas/*` |
| `backup.rs` | `/api/backup/*`, `/api/restore` |
| `curator.rs` | `/api/curator/*` |
| `curator_decisions.rs` | `/api/curator-decisions/*`, `/api/skills/*/curator-decisions` |
| `services.rs` | `/api/services/*`, `/api/containers/*` |
| `webhooks.rs` | `/api/webhooks/*`, `/webhook/*` |
| `oauth.rs` | `/api/oauth/*`, `/api/agents/*/oauth/*` |
| `email_triggers.rs` | `/api/triggers/email/*` |
| `github_repos.rs` | `/api/agents/*/github/repos/*` |
| `access.rs` | `/api/access/*` |
| `notifications.rs` | `/api/notifications/*` |
| `csp.rs` | `POST /api/csp-report` |
| `media.rs` | `/uploads/*`, `/api/media/*` |
| `workspace_files.rs` | `/workspace-files/{*path}?sig=&exp=` |
| `workspace.rs` | `/api/workspace/*` |

### Middleware Stack (outer to inner)

1. **Static files** — `ServeDir` for UI `out/` directory
2. **Request rate limit** — per-IP sliding window, default 300 rpm (`limits.max_requests_per_minute`)
3. **CSP report rate limit** — separate limit for `/api/csp-report`
4. **Webhook rate limit** — separate limit for `/webhook/*`
5. **Auth middleware** — Bearer token check; authenticated requests exempt from request rate limit. Auth lockout: 500 failed attempts → 30s block for requests without Authorization. Loopback exempt.
6. **W3C Trace Context** (`trace_context` module) — parses `traceparent` header, injects into tracing span
7. **CORS** — derives allowed origins from listen address; configurable via `cors_origins`

### SSE Event Types (Vercel AI SDK v3 compatible)

```
data-session-id      → {sessionId}
start                → {messageId?}
text-start           → {id?}
text-delta           → {delta}
text-end             → —
tool-input-start     → {toolCallId, toolName}
tool-input-delta     → {toolCallId, inputTextDelta}
tool-input-available → {toolCallId, input}
tool-output-available→ {toolCallId, output}
file                 → {url, mediaType?}
rich-card            → {cardType, data}
sync                 → {content, toolCalls, status, error?}
tool-approval-needed → {approvalId, toolName, args}
tool-approval-resolved → {approvalId, approved}
reconnecting         → —
usage                → {inputTokens, outputTokens, ...}
finish               → {finishReason, continuation}
error                → {errorText}
```

### `/api/health/dashboard`

Returns process-wide resilience metrics (Phase 62 + Phase 65):
- `sse_events_dropped_total` — per-agent, per-event-type backpressure drop counters
- `csp_violations` — per-directive CSP report counts
- `active_agents`, `sse_streams`, `approval_waiters`
- `auth_rate_limiter_size`, `request_rate_limiter_size`, `stream_registry_size`
- `db_pool_total`, `db_pool_idle`
- `memory_worker_heartbeat_age_secs` — seconds since memory worker last processed a task (-1 = unknown)
- `session_timeline_table_size_bytes`
- `uptime_secs`

---

## Memory System

All memory storage is in PostgreSQL. No separate vector DB process.

### Hybrid Search: 3-Way RRF

`MemoryStore.search()` (`crates/opex-core/src/memory/store.rs`) runs three branches in parallel via `tokio::join!` and merges with weighted Reciprocal Rank Fusion:

```
query
  ├─── embed(query) ──────► pgvector HNSW (cosine) ──► semantic_results (N×2 candidates)
  │                                                     (with MMR reranking, λ=0.75)
  ├─── tsvector tsquery ──► PostgreSQL FTS             ──► fts_results     (N×2 candidates)
  │                          (language-aware stemming, runtime-switchable)
  └─── pg_trgm similarity ► GIN trigram index          ──► trgm_results    (N×2 candidates)
                             (threshold 0.3)
                                       │
                                       ▼
              Weighted RRF: score(doc) = W_SEM × 1/(60 + rank_sem + 1)
                                       + W_FTS × 1/(60 + rank_fts + 1)
                                       + W_TRGM × 1/(60 + rank_trgm + 1)
              Weights: W_SEM=0.6, W_FTS=0.25, W_TRGM=0.15
```

**Single-branch shortcut:** if only one branch returns results, returns those directly (no RRF computation needed).

**Degradation:** if embedding endpoint is unavailable → FTS-only (mode returned as `"fts"`). If FTS AND-mode returns nothing → retries with OR-mode (`"fts_or"` for multi-word queries). Mode string included in tool output.

**MMR reranking** on semantic candidates (applied before RRF):
- Fetch `limit × 6` semantic candidates
- λ=0.75: balance relevance vs diversity
- `score = λ × (similarity × relevance_score) - (1-λ) × max_sim_to_selected`
- Inter-result similarity approximated as `min(candidate_sim, selected_sim)` (no cross-embeddings)

**FTS language** — runtime-switchable via API (default: "russian"). Stored in `RwLock<String>` in `MemoryStore`. Validated before SQL interpolation (regconfig cannot be parameterized).

**pg_trgm** (migration m035): `CREATE EXTENSION pg_trgm` + GIN index on `memory_chunks.content`. Threshold 0.3 (pg_trgm default). Added as third search branch in Sprint 1 P0.4.

### Two-Tier Memory

| Tier | `pinned` | Behavior |
|------|---------|----------|
| Raw | `false` | Temporal decay: daily cron 03:00 UTC — `relevance_score *= exp(-0.693/30 * days_since_accessed)` (30-day half-life). Cleanup cron 08:00 UTC: delete where `relevance_score < 0.1` AND `accessed_at > 180 days` |
| Pinned | `true` | Never decayed, never deleted by decay job. Permanent facts. |

Both tiers searched together. `pinned` flag surfaced in search results.

### Memory Watcher (`memory/watcher.rs`)

File watcher (notify crate) monitors workspace file `Create`/`Modify` events. On change: re-indexes the file as `scope='shared'` by calling `MemoryStore::index` **directly** — the embedding HTTP call and the `memory_chunks` upsert happen synchronously inside the watcher's tokio task, NOT through the `memory_tasks` queue / memory-worker process. The async task queue path is used only by (a) Core startup bootstrap reindex (when `memory_chunks` has zero `scope='shared'` rows) and (b) the explicit `POST /api/memory/reindex` endpoint. Watcher is delta-only (no initial scan).

### Memory Worker LISTEN/NOTIFY

Migration m023 adds a PostgreSQL trigger that calls `pg_notify('memory_tasks_new', '')` on every `INSERT` into `memory_tasks`. Memory worker holds a persistent `PgListener` on `memory_tasks_new` — wakes immediately on new tasks. Poll safety net fires every `poll_interval_secs` (default 60s) to reclaim tasks missed by a dropped LISTEN connection.

---

## Tool System

### Tool Types

```
tool_call(name, args)
       │
       ├── System tools ──── hardcoded in engine/engine_dispatch.rs
       │   workspace_write, workspace_edit, workspace_read, workspace_list,
       │   workspace_delete, workspace_rename, memory_write, memory_search,
       │   memory_get, memory_delete, web_fetch, code_exec, process_start,
       │   agent, canvas, rich_card, git_*, skill_*, tool_*, cron_*, etc.
       │
       ├── YAML tools ────── loaded from workspace/tools/*.yaml
       │   HTTP API calls with optional response transforms and channel actions
       │
       └── MCP tools ─────── Docker containers via bollard (McpRegistry)
           name prefixed with "mcp:" or resolved via McpRegistry
```

### Tool Policy

Evaluation order (deny wins):

```
1. agent.tools.deny[] → "tool is denied" (checked FIRST, before everything)
2. HookRegistry.fire(BeforeToolCall) → Block(reason) or Continue
3. agent.hooks.block_tools[] → silent deny
4. System tool → execute in engine (Rust)
5. YAML tool → yaml_tool_runner
6. MCP tool → McpRegistry
7. ToolRegistry fallback
```

Draft YAML tools (`status: draft`) excluded from tool list unless `include_draft = true`. Disabled tools (`status: disabled`) never loaded.

**Tool groups** — `[agent.tools.groups]` in agent TOML toggle entire groups (git, tool_management, etc.) to save LLM context tokens.

**`max_tools_in_context`** — when total tools exceed this limit, keyword matching against user message selects the most relevant subset.

### YAML Tool Execution (`yaml_tool_runner.rs`)

Each `workspace/tools/*.yaml` defines one tool. 30-second in-memory cache to avoid per-batch disk reads.

```
1. Resolve parameters (LLM-provided → default_from_env → default literal)
2. Build HTTP request (path/query/header/body params with template substitution)
3. Auth:
   bearer_env | basic_env | api_key_header | api_key_query |
   custom (${VAR} substitution) | oauth_refresh | oauth_provider | none
4. Execute via reqwest with **conditional SSRF**: `engine_dispatch.rs` checks the tool's `endpoint` against `tools::ssrf::is_internal_endpoint`. Internal endpoints (toolgate:9011, browser-renderer, etc. — admin-configured and trusted) use the standard `http_client()`. External endpoints (any URL not on the internal allow-list) use `ssrf_http_client()` with private-IP blocking.
5. response_transform: optional JSONPath extraction ("$.path.to.field")
6. If channel_action → route binary result to ChannelActionRouter instead of LLM
7. Return text result to LLM context
```

Auth keys resolved via `SecretsEnvResolver`: agent-scoped → global → env var fallback.

`required_base: true` — tool only available to agents with `base = true`.

### SSRF Guard (`crates/opex-core/src/net/ssrf.rs`)

Phase 64 unified guard for user-supplied URLs (`web_fetch`, `fetch_url_content`):

**Layer 1 — sync pre-check** (`validate_url_scheme()`):
- Only `http://` and `https://` schemes allowed
- Internal service blocklist by `host:port` (toolgate:9011, postgres, Docker socket, SearXNG, browser-renderer, core itself)
- Numeric private IPs in URL blocked immediately

**Layer 2 — DNS-time filtering** (`SsrfSafeResolver`):
- Custom `reqwest::dns::Resolve` implementation
- After DNS resolution: filters out all private/internal IPs
  - IPv4: RFC 1918 (10/8, 172.16/12, 192.168/16), loopback (127/8), link-local (169.254/16), CGNAT (100.64/10), multicast (224/4), broadcast, unspecified
  - IPv6: loopback (::1), unspecified (::), ULA (fc00::/7), link-local (fe80::/10), Teredo (2001::/32), 6to4 (2002::/16), multicast (ff00::/8), IPv4-mapped (::ffff:x.x.x.x)
- If ALL resolved addresses are private → `PermissionDenied` (connection never attempted)
- Closes DNS-rebinding TOCTOU gap

**Conditional SSRF for YAML tools**: the runtime checks `tools::ssrf::is_internal_endpoint(&yaml_tool.endpoint)`. Endpoints recognised as internal (toolgate, browser-renderer, core itself, etc.) use the standard `http_client()`; everything else uses `ssrf_http_client()` with private-IP blocking. Toolgate's own outbound calls share the same logic.

**`ssrf_http_client()`** — canonical safe client: 30s default timeout, 10s connect timeout, redirect policy NONE, `SsrfSafeResolver`.

### MCP (Model Context Protocol)

MCP servers run as Docker containers managed via bollard. `McpRegistry` wraps `ContainerManager`:

```
LLM requests tool "mcp_name__tool_name"
       │
       ▼
McpRegistry.call_tool("mcp_name", "tool_name", args)
       │
       ├── ContainerManager.ensure_running("mcp_name")
       │   ├── check if container already running
       │   ├── if not: docker pull + docker run (bollard async API)
       │   └── return base_url (e.g. "http://container-ip:8080")
       │
       └── POST {base_url}/mcp
           {"jsonrpc":"2.0","method":"tools/call","params":{"name":"tool_name","arguments":args},"id":2}
           └── parse JSON-RPC 2.0 response → tool result string
```

Tool definitions discovered via `tools/list`, cached in `RwLock<HashMap<String, Vec<ToolDefinition>>>`. Cache invalidated on container restart. MCP configs also loadable from `workspace/mcp/*.yaml`.

---

## Channel System

### WebSocket Loopback Architecture

Channel adapters connect to Core via `GET /ws/channel/{agent_name}`. All communication is serialized JSON over the internal WebSocket:

```
┌─────────────────────────────────────────────────────────────────┐
│                       opex-core                                 │
│                                                                 │
│   channels process (Bun)              AgentEngine              │
│   ┌───────────────────────────┐       ┌─────────────────────┐  │
│   │  grammy / discord.js etc. │       │  tool handlers       │  │
│   │  listens on external API  │       │                     │  │
│   │          │                │       │  ChannelActionRouter │  │
│   │          ▼                │       │         │           │  │
│   │  ChannelInbound (JSON) ───┼──────►│  IncomingMessage    │  │
│   │                           │       │  (via WS loopback)  │  │
│   │  ChannelOutbound ◄────────┼───────│  ChannelAction      │  │
│   │  (send_voice, react, etc.)│       │  (mpsc ch. cap 64)  │  │
│   └───────────────────────────┘       └─────────────────────┘  │
│            ▲                                                    │
│            │ WS frames (JSON)                                   │
│   /ws/channel/{agent_name}                                     │
└─────────────────────────────────────────────────────────────────┘
```

### Inbound Message Flow

```
1. Adapter receives external message (e.g. Telegram update via grammy)
2. Serialize to ChannelInbound JSON: {text, attachments, context, sender_id, ...}
3. Send JSON frame over /ws/channel/{agent_name}
4. Gateway WS handler deserializes to IncomingMessage
   └── context: opaque serde_json::Value (engine never inspects it — Immutable Core principle)
5. AccessGuard.is_allowed(sender_id) check
6. engine.handle_with_status() called in spawned task
7. Response text sent back as ChannelOutbound frame
8. Adapter formats and delivers to user
```

### Outbound Channel Actions

When an engine tool produces a channel action (`channel_action:` in YAML tool config):
```
1. YAML tool executes HTTP call → receives binary payload (audio, image, etc.)
2. channel_action config triggers ChannelActionRouter.send()
3. ChannelActionRouter looks up adapter by "{channel_type}:{uuid}"
4. Sends ChannelAction {name, params, context} via bounded mpsc (capacity 64)
5. Adapter's action receiver loop picks it up
6. Adapter performs platform-specific action (upload file, set reaction, etc.)
7. Result sent back via oneshot::Sender<Result<(), String>>
```

`ChannelActionRouter` maintains `RwLock<HashMap<String, mpsc::Sender<ChannelAction>>>` keyed by `"{channel_type}:{uuid}"`. UUID is generated fresh on each WebSocket connection to prevent stale senders.

### DM Pairing and Mirroring

Channels support DM pairing: `POST /api/channels/notify` can deliver messages to specific channels by UUID. This is used by:
- Watchdog alerts
- Cron job delivery targets
- Scheduler `announce_to` field

Session mirroring (`sessions.mirror_to_session()`, migration m043): cron delivery writes `is_mirror = true` messages to the target session as passive context, without affecting `last_message_at`.

### Access Guard

`AccessGuard` enforces per-agent access control:
- `mode: "open"` — all users allowed (default)
- `mode: "restricted"` — only `owner_id` and users in `access` DB table
- Pairing codes: owner generates time-limited codes for granting access

---

## Scheduler

### `Scheduler` struct (`crates/opex-core/src/scheduler/mod.rs`)

```rust
struct Scheduler {
    scheduler: JobScheduler,         // tokio-cron-scheduler
    dynamic_jobs: RwLock<HashMap<Uuid, Uuid>>,   // DB job id → scheduler UUID
    agent_jobs: RwLock<HashMap<String, Vec<Uuid>>>, // agent name → heartbeat UUIDs
    ui_event_tx: broadcast::Sender<String>,
    agent_locks: AgentLocks,         // Arc<Mutex<HashSet<String>>> — per-agent exec lock
    backup_job: RwLock<Option<Uuid>>,
    curator_job: RwLock<Option<Uuid>>,
}
```

### Heartbeat

```toml
[agent.heartbeat]
cron = "*/30 10-19 * * *"
timezone = "UTC"
announce_to = "telegram"
```

Execution: per-agent lock check → construct synthetic `IncomingMessage {channel: "heartbeat"}` → `engine.handle_isolated()` → if response ≠ `"HEARTBEAT_OK"` → deliver to `announce_to`.

**Cron normalization:** 5-field cron prepended with `"0 "` (seconds). Local timezone hours converted to UTC.

### Dynamic Cron Jobs

Stored in `scheduled_jobs` table. Hot-loaded into running scheduler without restart.

`ScheduledJob` fields:
```
id, agent_id, name, cron_expr, timezone, task_message, enabled,
created_at, last_run_at, silent, announce_to (JSONB), jitter_secs,
run_once, run_at, tool_policy (JSONB override)
```

### DeliveryTarget (`announce_to`)

`normalize_announce_to(val)` normalizes `announce_to` JSONB into flat `Vec<Value>` of delivery targets:

| Input form | Resolved as |
|-----------|-------------|
| `"local"` | `{"type": "local"}` → save to `workspace/agents/{agent}/cron_output/` |
| `"telegram:12345"` | `{"channel": "telegram", "chat_id": 12345}` |
| `"telegram:12345:67890"` | same (thread_id dropped, future work) |
| Object | pass through |
| Array | each item processed individually |

**`save_to_local()`** persists cron reply to `workspace/agents/{agent}/cron_output/{YYYYMMDDTHHMMSS}_{job_short}.txt`. Long replies (>4000 chars) truncated for channel delivery with workspace save notification.

### Built-in Scheduler Jobs

| Job | Schedule | Description |
|-----|----------|-------------|
| Memory decay | 03:00 UTC daily | `relevance_score *= exp(-decay)` on raw chunks |
| Memory cleanup | 08:00 UTC daily | Delete chunks with `relevance_score < 0.1` AND >180 days old |
| Session cleanup | hourly | Prune `session_timeline` rows older than `retention_days` |
| Session age prune | 05:00 UTC daily | Delete oldest sessions exceeding `max_sessions_per_agent` |
| Backup | configurable (default 05:00 UTC) | `pg_dump` via Docker container |
| Curator | configurable (default Sunday 03:00) | Skill review and repair pipeline |

### Per-Agent Execution Lock

`AgentLocks: Arc<Mutex<HashSet<String>>>` shared across all scheduled jobs. Before executing any heartbeat or cron task: insert agent name into set; remove on completion. If name already present: skip tick. Prevents concurrent scheduled executions from same agent.

---

## Database Schema

PostgreSQL 17 + pgvector. Migrations in `migrations/*.sql` (sqlx). Auto-run on startup. No ORM — raw sqlx queries in `crates/opex-core/src/db/`.

**Current migration state:** m001 through m051 (latest migration in `migrations/`); some numbers in the sequence were never committed (count of `.sql` files is the source of truth).

### Key Tables

| Table | Description | Notable columns |
|-------|-------------|-----------------|
| `sessions` | Chat sessions | `id`, `agent_id`, `status` (running/done/failed/interrupted), `parent_session_id` (compression chain), `end_reason` ('compression' or NULL), `compaction_state` (JSONB with `CompressorState`) |
| `messages` | Individual messages | `id`, `session_id`, `role`, `content`, `parent_message_id`, `branch_from_message_id`, `is_mirror` (bool) |
| `session_timeline` | chronological lifecycle log | `session_id`, `event_type` (running/tool_start/tool_end/done/failed), `created_at` |
| `session_failures` | Terminal failure log | `session_id`, `failure_kind`, `error_message`, `last_tool_name`, `llm_provider`, `llm_model`, `iteration_count` |
| `memory_chunks` | Vector memory | `id`, `agent_id`, `scope` (agent name/'shared'), `content`, `embedding` (halfvec), `fts_vector` (tsvector), `relevance_score`, `pinned`, `accessed_at` |
| `memory_tasks` | Worker task queue | `id`, `task_type`, `status` (pending/processing/done/failed), `payload` (JSONB) |
| `secrets` | Encrypted secrets | `name`, `scope`, `encrypted_value` (bytea), `nonce` (bytea 12 bytes) |
| `providers` | LLM provider configs | `name`, `provider_type`, `base_url`, `default_model`, `api_key_env` |
| `provider_active` | Active provider per capability | `capability` (stt/tts/vision/imagegen/embedding), `provider_id` |
| `scheduled_jobs` | Dynamic cron jobs | `id`, `agent_id`, `cron_expr`, `timezone`, `task_message`, `announce_to` (JSONB), `jitter_secs`, `run_once`, `run_at`, `tool_policy` (JSONB) |
| `agent_channels` | Channel configs | `id`, `agent_id`, `channel_type`, `config` (JSONB — credentials redacted, stored in vault) |
| `usage_log` | Token usage per session turn | `session_id`, `agent_id`, `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `reasoning_tokens` (all nullable) |
| `approvals` | Pending tool approvals | `id`, `session_id`, `tool_name`, `args`, `status`, `expires_at` |
| `webhooks` | Outbound webhooks | `id`, `agent_id`, `url`, `events` |
| `notifications` | UI notifications | `id`, `type`, `title`, `body`, `data` (JSONB), `read_at` |
| `system_flags` | Feature flags | `key`, `value` (e.g. `setup_complete`) |
| `curator_runs` | Skill curator history | `id`, `trigger`, `status`, `phase1/2/3`, `report_md` |
| `curator_decisions` | Per-skill curator decisions | `skill_id`, `decision`, `reason`, `created_at` |
| `access` | Per-agent user access list | `agent_id`, `channel_user_id` |
| `pairing_codes` | Temporary access codes | `code`, `agent_id`, `expires_at` |

### Token Columns (migration m036)

Extended token tracking added to `usage_log`:
- `cache_read_tokens` — tokens read from prompt cache (Anthropic `cache_read_input_tokens`, OpenAI `cached_tokens`, Gemini `cachedContentTokenCount`). Subset of `input_tokens`. Never sum to base.
- `cache_creation_tokens` — tokens written to prompt cache (Anthropic only). Cost ×1.25 base input.
- `reasoning_tokens` — hidden reasoning tokens (o1/o3, DeepSeek-R1, Gemini thinking). Subset of `output_tokens`.

---

## Secrets Vault

ChaCha20-Poly1305 encryption (pure Rust, no OpenSSL).

```
secrets table
  PK: (name, scope)
  encrypted_value: bytea  (ciphertext + 16-byte auth tag)
  nonce: bytea            (12 bytes, random per write)
```

Each write generates a unique 12-byte random nonce. Master key (`OPEX_MASTER_KEY`, 32-byte hex) never stored in DB. In-memory cache: `RwLock<HashMap<(name, scope), String>>`.

**Scoped resolution** (`get_scoped(name, scope)`):
```
1. (name, scope)  — agent-specific (scope = agent name)
2. (name, "")     — global secret (scope = empty string)
3. env::var(name) — environment variable fallback
```

Channel credentials (`bot_token`, `access_token`, etc.) extracted from `agent_channels.config` on create/update and stored in vault under key `CHANNEL_CREDENTIALS`, scope = channel UUID. `config` column in DB never contains credential values.

Agent rename migrates scoped secrets via `rename_scope(old, new)`.

---

## Security

### Upload URL Signing (Phase 64 SEC-03)

`GET /uploads/*` and `/workspace-files/*` URLs are HMAC-signed:
- `UploadsConfig { signed_url_ttl_secs: 86400, require_signature: bool }`
- `require_signature = false` by default (v0.19.0 grace period)
- `mint_workspace_file_url()` generates signed URL with expiry
- Workspace write tool emits `__file__:` markers with signed URLs

### Approval Workflow

```
tool call → needs_approval(approval_config, tool_name) → true
       │
       ▼
create_approval(db, session_id, tool_name, args) → approval_id (UUID)
       │
       ▼
emit StreamEvent::ApprovalNeeded {approval_id, tool_name, args}
       │
       ▼
wait approval_waiter.wait(timeout) → oneshot::Receiver
       │                              (tokio::sync::oneshot, registered in ApprovalManager)
       ▼
POST /api/approvals/{id}/approve (or /reject) wakes waiter
       │
       ▼
continue with result or return rejection error
```

Approval timeout: `approval.timeout_seconds` (default 300 = 5 min).

### DelegationConfig

Per-agent subagent delegation policy:
```toml
[agent.delegation]
max_depth = 1                    # subagents CANNOT recursively spawn further subagents
blocked_tools_extra = [...]      # extends built-in deny-list (used at runtime)
```

`SUBAGENT_DENIED_TOOLS` is the runtime safety net for every subagent. The
runtime gate (`runtime_subagent_denylist`) hard-anchors that constant and
only honours `blocked_tools_extra` (additive). A subagent author cannot
gain access to `cron`, `secret_set`, `process`, `code_exec`,
`workspace_delete`, or `workspace_rename` — they remain blocked everywhere.
Audit 2026-05-08, groups T (5th pass) and FF (6th pass).

Base subagents (`agent.base = true`) get a runtime carve-out for `code_exec`
because base agents are documented as the host-level operator role
(`scaffold/base/SOUL.md`); the carve-out applies only when the SUBAGENT itself
is base, not when a base parent spawns a non-base peer.

### Rate Limiting

- **Auth lockout**: 500 failed auth attempts → 30s block for requests without Authorization header. Loopback exempt. (`AuthRateLimiter(500, 30)`)
- **Request rate limit**: 300 rpm default, per-IP sliding window. Authenticated requests exempt. Configurable via `limits.max_requests_per_minute`.
- **CSP report rate limit**: separate limit for `/api/csp-report`
- **Webhook rate limit**: separate limit for `/webhook/*`

All rate limiter instances stored in `OnceLock<Arc<T>>` module-level statics (Phase 66 REF-06, replacing intentional-leak pattern).

### Tool Name Validation

API handlers enforce `[a-zA-Z0-9_-]` on tool and MCP entry names — prevents path traversal in `workspace/tools/` lookup.

---

## Observability

### Structured Logging

`tracing` crate with `tracing-subscriber` JSON output. Broadcast to connected WebSocket clients (`/ws`) via `BroadcastLogLayer` for the UI logs page.

Example:
```rust
tracing::info!(agent = %name, session = %session_id, tool = %tool_name, "hook: tool call");
```

### OpenTelemetry (optional)

Requires `otel` feature flag. Config:
```toml
[otel]
enabled = true
service_name = "opex-core"    # default
```
Set `OTEL_EXPORTER_OTLP_ENDPOINT` env var for collector address. `tracing-opentelemetry` bridges tracing spans to OTLP.

### W3C Trace Context (Phase 65 OBS-04)

`trace_context` middleware (`opex_gateway_util::trace_context`) parses `traceparent` header from inbound requests and injects it into the tracing span context, enabling distributed trace propagation across services.

### Metrics Registry (Phase 65 OBS-02)

`Arc<MetricsRegistry>` in `AgentConfig.metrics`:
- `record_tool_latency(tool_name, agent_name, status, elapsed)` — tool call latency histogram
- `record_llm_call(agent_name, duration, input_tokens, output_tokens)` — LLM call duration
- `sse_events_dropped_total` — per-agent, per-event-type SSE backpressure drops (surfaced at `/api/health/dashboard`)

---

## Process Manager

`ProcessManager` (`crates/opex-core/src/process_manager/`) manages long-lived child processes.

### Spawn

```rust
tokio::process::Command::new(exe)
    .args(args)
    .current_dir(working_dir)
    .envs(env)              // minimal: only env_passthrough keys + env_extra literals
    .process_group(0)       // own process group → SIGKILL kills grandchildren
    .spawn()
```

Environment is minimal: only explicitly listed `env_passthrough` keys forwarded, plus `env_extra` literals (with `${VAR}` substitution). DB credentials never forwarded to channel adapters.

### Monitor Loop

Background `tokio::spawn` polls each child's `try_wait()` every 5 seconds. On unexpected exit: respawn with exponential backoff. `POST /api/services/{name}/restart` kills and respawns on demand.

**Container restart whitelist** — only non-sensitive containers restartable via API: browser-renderer, searxng, mcp-*. PostgreSQL is excluded.

### Graceful Shutdown

On `SIGTERM`/`SIGINT`:
1. `tokio::signal::unix::signal(SIGTERM)` fires
2. Drain in-flight agents (`handle.shutdown()` on each, up to `drain_timeout_secs` default 30s)
3. `ProcessManager.stop_all()`: SIGTERM to process groups, wait 5s, SIGKILL
4. DB pool closes

systemd `TimeoutStopSec` should be `drain_timeout_secs + 10s buffer` (40s default).

---

## UI Architecture

### Stack

| Layer | Technology |
|-------|-----------|
| Framework | Next.js 16 (App Router) + React 19 |
| State management | Zustand 5 + Immer |
| UI components | shadcn/ui (Radix + Tailwind 4) |
| Data fetching | TanStack Query v5 (with persist client) |
| Rich text | TipTap v3 (workspace markdown editor) |
| Code editor | CodeMirror 6 |
| Graph visualization | Sigma.js (react-sigma) + Dagre layout |
| Internationalization | Custom i18n (en/ru) |

### Chat Store Decomposition (Phase 54)

`chat-store.ts` (451 lines) is the central SSE state machine. Supporting modules:
- `chat-types.ts` — `ChatMessage`, `MessagePart`, `AgentState`, `ConnectionPhase`, `MessageSource`
- `chat-history.ts` — `convertHistory`, `resolveActivePath`, `findSiblings`, `getCachedRawMessages`
- `chat-reconciliation.ts` — `contentHash`, `reconcileLiveWithHistory`
- `chat-persistence.ts` — `saveLastSession`, `getLastSessionId`, `getInitialAgent` (localStorage)
- `streaming-renderer.ts` — factory via `createStreamingRenderer()`: SSE parsing, rAF throttling (50ms), reconnection, per-agent Map cleanup. Non-serializable state (AbortController, setTimeout) in private closures, not Immer

### Static Export with RSC Flattening

Built as static export (`next build` → `out/`) for nginx. RSC chunks are
flattened automatically by `ui/build/adapter.cjs`, registered in
`next.config.ts` via `experimental.adapterPath` — no separate post-build
script needed.

---

## Configuration

**Three required `.env` keys (only these):**
- `OPEX_AUTH_TOKEN` — HTTP API bearer token
- `OPEX_MASTER_KEY` — vault encryption key (32-byte hex)
- `DATABASE_URL` — PostgreSQL connection string

All other configuration goes in `config/opex.toml` or the secrets vault.

**Key config files:**
- `config/opex.toml` — `AppConfig`: gateway, database, limits, subagents, memory, docker, otel, managed_process, agent defaults, backup, curator, cleanup, shutdown, uploads, agent_tool timeouts
- `config/agents/{Name}.toml` — per-agent `AgentConfig` (case-sensitive filename = agent name)
- `workspace/tools/*.yaml` — YAML tool definitions
- `workspace/skills/*.md` — shared agent skills
- `config/skills/*.md` — system skills (base agents only)
- `workspace/mcp/*.yaml` — MCP server definitions
- `config/services/*.yaml` — service registry for infrastructure endpoints (browser-renderer, toolgate). STT/TTS/embedding/vision are NOT here — they go through the provider registry (`providers`/`provider_active` tables) proxied by toolgate.

**Hot reload:** config file watcher (notify crate) reloads `config/opex.toml` and agent configs without restart. Agent rename is transactional (updates 19+ DB tables).

**Agent deletion:** a three-class table classification (Ephemeral / History / DropRipe) in `gateway/handlers/agents/crud.rs` drives complete cleanup — Ephemeral tables deleted, History kept unless `?purge_history=true`, `memory_chunks` private-scope deleted + shared-scope anonymized, soul biography backed up fail-closed before any destructive step, workspace dir removed with a canonicalize guard. A drift test + `agent_table_classification` doctor check catch unclassified new tables. Operator runbook: `docs/runbooks/agent-deletion.md`.

For complete configuration reference see `docs/CONFIGURATION.md`.
