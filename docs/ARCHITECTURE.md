# HydeClaw Architecture

## Overview

HydeClaw is a self-hosted AI gateway and multi-agent platform written in Rust. The primary deployment target is a Raspberry Pi 4 (ARM64). The stack consists of three Rust binaries (`hydeclaw-core`, `hydeclaw-watchdog`, `hydeclaw-memory-worker`) running as independent systemd services, with two managed child processes (`channels` in Bun/TypeScript, `toolgate` in Python/FastAPI) spawned by core.

```
┌──────────────────────────────────────────────────────────────────────────┐
│                          hydeclaw-core (systemd)                         │
│                                                                          │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐   │
│  │  AgentEngine │  │  AgentEngine │  │  Scheduler   │  │  Gateway   │   │
│  │  "Agent1"    │  │  "Agent2"    │  │  (cron jobs) │  │  (axum)    │   │
│  └──────┬───────┘  └──────┬───────┘  └──────────────┘  └─────┬──────┘   │
│         │                 │                                    │          │
│  ┌──────▼─────────────────▼────────────────────────────────────▼──────┐   │
│  │             ChannelActionRouter  /  ProcessingTracker              │   │
│  └───────────────────────────────────────────────────────────────────┘   │
│                                                                          │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────┐  ┌─────────────┐   │
│  │ SecretsVault│  │ MemoryStore  │  │ McpRegistry │  │ToolRegistry │   │
│  │ ChaCha20    │  │ pgvector+FTS │  │ (bollard)   │  │ RwLock<Map> │   │
│  └─────────────┘  └──────────────┘  └─────────────┘  └─────────────┘   │
└───────────────────────────┬──────────────────────────────────────────────┘
                            │  manages child processes
                  ┌─────────┴─────────┐
                  │                   │
         ┌────────▼───────┐   ┌───────▼──────┐
         │  channels      │   │  toolgate    │
         │  (Bun/TS)      │   │  (Python)    │
         │  WebSocket ↔   │   │  port 9011   │
         │  hydeclaw-core │   └──────────────┘
         └────────────────┘

┌──────────────────┐  ┌──────────────────┐
│ hydeclaw-watchdog│  │ hydeclaw-memory- │
│ (systemd)        │  │ worker (systemd) │
│ health monitor   │  │ background index │
└────────┬─────────┘  └────────┬─────────┘
         │                     │
         └──────────┬──────────┘
                    │
     ┌──────────────▼──────────────────┐
     │      PostgreSQL 17 + pgvector    │
     │  sessions, messages,             │
     │  memory_chunks, secrets,         │
     │  tasks, agent_channels           │
     └─────────────────────────────────┘
```

**Key binaries:**

| Binary | Role |
|--------|------|
| `hydeclaw-core` | Main process: HTTP gateway, agent engines, scheduler, secrets, MCP |
| `hydeclaw-memory-worker` | Separate binary: background indexing task queue (poll-based) |
| `hydeclaw-watchdog` | Optional: external health monitor for the core process |

**Stack:** axum, tokio (4 worker threads), sqlx (async PostgreSQL), bollard (Docker), reqwest (rustls, no OpenSSL). Binary size ~14 MB, idle RAM ~2.2 MB.

---

## Agent Engine

### Entry Points

Every agent has one `AgentEngine` instance, owned by an `Arc<AgentEngine>`. There are two ways to invoke it:

1. **HTTP SSE** (`POST /api/chat`): called from the UI or API. The engine streams `StreamEvent` variants over Server-Sent Events using the Vercel AI SDK UI Message Stream Protocol v1. The gateway handler calls `engine.handle_with_status()` and pipes `mpsc::Receiver<StreamEvent>` to the SSE response.

2. **Internal `handle_isolated()`**: called by the Scheduler for cron jobs and heartbeats. Creates a fresh, throw-away session in the DB so cron runs never accumulate context from previous invocations. Also used by `agent` tool for inter-agent communication.

### LLM Loop Flow

The core loop runs inside `handle_isolated()` (for internal calls) or the SSE path (for user-facing calls). The steps are identical:

```
1. build_context()
   ├── load system prompt from workspace/AGENTS.md + USER.md
   ├── inject runtime context: agent name, model, channel, datetime, channel list
   ├── load session history from DB (last N messages)
   ├── prune_old_tool_outputs(): replace tool results in old turns with "[omitted]"
   └── assemble available_tools: system + YAML (cached 30s) + MCP

2. enrich_message_text()
   ├── PII redaction (credit cards, phone numbers, etc.)
   ├── URL auto-fetch (up to 2 URLs, 10s timeout, 512 KB limit)
   ├── attachment enrichment (description strings from media metadata)
   └── audio auto-transcription via Toolgate STT

3. LLM call  →  provider.chat() or provider.chat_stream()
   ├── ThinkingFilter: strips <think>/<thinking>/<thought>/<antthinking> blocks in-stream
   └── retry on TransientHttp / Overloaded (3 attempts, exponential backoff)

4. Response branch:
   ├── tool_calls.is_empty() == true  →  final response, break
   │   └── auto-continue check: if looks_incomplete(), inject nudge and continue
   └── tool_calls present  →  execute_tool_calls_partitioned() → append results → goto 3

5. Post-loop:
   ├── save final assistant message to DB
   ├── session graph extraction (background tokio::spawn, ≥5 messages)
   └── emit SSE Finish event
```

The loop runs up to `max_iterations` times (default: 50). If the loop detector fires or the iteration limit is hit, a forced final LLM call is made with no tools available so the model produces a conclusion.

### Tool Execution Partitioning

Tool calls returned by the LLM in a single turn are partitioned into two groups before execution:

**Parallel-safe** (read-only or independently stateful):

```
web_fetch, memory_search, memory_get, workspace_read, workspace_list,
tool_list, skill_list, sessions_list, sessions_history, session_search,
session_context, session_export, canvas, rich_card, agent
```

YAML tools with `parallel: true` and no `channel_action` are also eligible for the parallel batch.

**Sequential** (write operations, tools requiring approval, channel actions):

Everything else executes serially, in order, one at a time.

The parallel batch runs via `futures_util::future::join_all()`. Both parallel and sequential calls are subject to a 120-second per-tool timeout.

**Context enrichment:** before dispatch, each tool call receives a `_context` field injected by the engine (not the LLM) containing the session ID, channel identifier, and the original `IncomingMessage.context` blob. This prevents the LLM from forging channel context (e.g., spoofing a `chat_id` for Telegram actions).

### SSE Streaming Event Flow

```
Engine                    Gateway SSE handler           UI (chat-store.ts)
  │                             │                             │
  │── StreamEvent::SessionId ──>│── data: {type:"data-       │
  │                             │   session-id",...}  ──────>│
  │── StreamEvent::MessageStart─>│── data: {type:"start"}───>│
  │── StreamEvent::TextDelta ──>│── data: {type:"text-delta"}│ (streaming text)
  │   (loop, chunk by chunk)    │                             │
  │── StreamEvent::ToolCallStart>│── data: {type:"tool-      │
  │                             │   input-start",...}  ─────>│ (spinner)
  │── StreamEvent::ToolResult  ─>│── data: {type:"tool-      │
  │                             │   output-available",...}──>│
  │── StreamEvent::RichCard    ─>│── data: {type:"rich-card"} │ (inline table/metric)
  │── StreamEvent::File        ─>│── data: {type:"file",...} ─>│ (inline media)
  │── StreamEvent::Finish      ─>│── data: {type:"finish"}   │
  │                             │   close SSE stream         │
```

The UI `chat-store.ts` parses each `data:` line with `parseSseEvent()` and applies mutations to the per-agent `UIMessage[]` state using Immer, accumulating deltas into `TextPart`, `ToolPart`, `RichCardPart`, and `FilePart` message parts.

### Thinking Filter

Two modes depending on context:

- **Batch (post-LLM):** `strip_thinking(text)` — scans for `<think>`, `<thinking>`, `<thought>`, `<antthinking>` open tags (ASCII case-insensitive) and removes everything up to the matching close tag. Multiple blocks in one response are all removed. Unclosed blocks strip the remainder.

- **Streaming:** `ThinkingFilter` struct maintains stateful `in_thinking` flag. Buffers up to 20 bytes at the end of each chunk to handle partial open/close tags split across chunk boundaries.

**Override:** if the incoming message context contains `directives.think: true` (set by `/think` command), thinking blocks are preserved at levels 0–2. Level 3+ always preserves them regardless of directive.

### Error Classification and Retry

Errors from LLM provider calls are classified via regex patterns (compiled once with `LazyLock`) into 7 classes:

| Class | Trigger patterns | Recovery |
|-------|-----------------|----------|
| `Billing` | `402`, "payment required", "insufficient credit/quota/balance" | No retry, user error message |
| `AuthPermanent` | `401`/`403` + api key, "unauthorized", "api key invalid/revoked" | No retry, user error message |
| `ContextOverflow` | "context length", "token limit", "input too long", 上下文 | Compact context, retry |
| `SessionCorruption` | "tool_use_block", "roles must alternate", "orphan tool", "incorrect role" | Reset messages to system+user, retry once |
| `RateLimit` | `429`, "too many requests", "tokens per minute", "resource exhausted" | 60s cooldown |
| `Overloaded` | "overloaded", "high demand", "overloaded_error" | 30s cooldown |
| `TransientHttp` | `500`/`502`/`503`/`504`/`521`–`524`/`529`, "bad gateway", "gateway timeout" | 15s cooldown, up to 3 retries |
| `Unknown` | anything else | 15s cooldown |

Only `TransientHttp` and `Overloaded` are retried at the engine level (`is_retryable()`). All other classes surface a localized user-facing message.

### Agent Tool (Evolution from Handoff)

HydeClaw has transitioned from a linear `handoff` loop (where control was passed from agent to agent) to a **polling-based live agent pool** model. This provides better scalability and allows a single parent agent to drive multiple sub-tasks in parallel.

- **Legacy Handoff (Removed):** Used to pass control via synthetic user messages. Difficult to parallelize and prone to infinite loops.
- **Modern Agent Tool:** Spawns subagents as background tasks in a session-scoped pool. The parent agent uses `agent(action="ask", target=…, text=…)` — a single canonical "talk to a peer" verb that auto-spawns on pool-miss, continues the dialog on pool-hit, and blocks until a result is available. `status` and `kill` round out the API for inspection and explicit teardown. See [`workspace/skills/multi-agent-coordination.md`](../workspace/skills/multi-agent-coordination.md) for patterns.
- **Session Pooling:** Each session maintains its own `SessionAgentPool`, ensuring that agents are isolated and their lifecycle is tied to the current chat session.

The `handoff` name is preserved in some legacy configuration fields (e.g., `max_handoff_context_chars`) for backward compatibility with existing `hydeclaw.toml` files, but functionally it now refers to the context transfer between the initiator and the subagent.

### Inter-Agent Communication

`agent(action="ask")` routes through the `AgentMap` (a `Arc<RwLock<HashMap<String, Arc<AgentEngine>>>>` shared across all agents):

1. Looks up the target agent by name.
2. Looks up the target in the per-session `SessionAgentPool`. On miss, spawns a `LiveAgent` (background tokio task running `engine.run_subagent()`); on hit, sends `text` as the next user message in the existing dialog.
3. Blocks until the peer's loop finishes, returning its `last_result` to the caller.
4. Leaves the peer alive in the pool for follow-ups; explicit `kill` (or `fresh=true` on the next `ask`) frees the slot.

The `agent` tool is stripped from the available tool list when the incoming message is itself an inter-agent call, preventing broadcast loops.

### LLM Providers

28 provider types are defined in `PROVIDER_TYPES` (`providers.rs`):

| Provider key | Implementation | Auth env var |
|---|---|---|
| `openai` | OpenAI-compatible HTTP | `OPENAI_API_KEY` |
| `anthropic` | Anthropic Messages API (native) | `ANTHROPIC_API_KEY` |
| `google` / `gemini` | Google Generative AI REST | `GOOGLE_API_KEY` |
| `minimax` | OpenAI-compatible | `MINIMAX_API_KEY` |
| `deepseek` | OpenAI-compatible | `DEEPSEEK_API_KEY` |
| `groq` | OpenAI-compatible | `GROQ_API_KEY` |
| `together` | OpenAI-compatible | `TOGETHER_API_KEY` |
| `openrouter` | OpenAI-compatible | `OPENROUTER_API_KEY` |
| `mistral` | OpenAI-compatible | `MISTRAL_API_KEY` |
| `xai` | OpenAI-compatible | `XAI_API_KEY` |
| `perplexity` | OpenAI-compatible | `PERPLEXITY_API_KEY` |
| `ollama` | OpenAI-compatible, local | — |
| `claude-cli` | Subprocess (Claude CLI inside Docker sandbox) | — |
| `gemini-cli` | Subprocess (Gemini CLI inside Docker sandbox) | — |
| `openai_compat` | Generic OpenAI-compatible endpoint | `API_KEY` |
| `huggingface` | OpenAI-compatible | `HF_API_KEY` |
| `moonshot` | OpenAI-compatible (Kimi) | `MOONSHOT_API_KEY` |
| `nvidia` | OpenAI-compatible (NIM) | `NVIDIA_API_KEY` |
| `venice` | OpenAI-compatible | `VENICE_API_KEY` |
| `cloudflare` | OpenAI-compatible (AI Gateway) | `CF_AI_API_KEY` |
| `litellm` | OpenAI-compatible, local proxy | — |
| `volcengine` | OpenAI-compatible (Doubao) | `VOLCENGINE_API_KEY` |
| `qwen` | OpenAI-compatible (Alibaba DashScope) | `DASHSCOPE_API_KEY` |
| `glm` | OpenAI-compatible (Zhipu AI) | `GLM_API_KEY` |
| `sglang` | OpenAI-compatible, local | — |
| `vllm` | OpenAI-compatible, local | — |
| `qianfan` | OpenAI-compatible (Baidu) | `QIANFAN_API_KEY` |
| `xiaomi` | OpenAI-compatible (MiLM) | `XIAOMI_API_KEY` |

All providers implement `LlmProvider` trait (`chat()` + `chat_stream()`). API keys are resolved from `SecretsManager` on each call, making them hot-reloadable without restart.

---

## Memory System

Memory is stored entirely in PostgreSQL. There is no separate vector database process.

### Hybrid Search

Every search query runs semantic and FTS in parallel (`tokio::join!`) and merges via Reciprocal Rank Fusion:

```
query
  │
  ├─── embed(query) ──────> pgvector HNSW index  ──> semantic_results (N×2 candidates)
  │                          (cosine distance)
  └─── tsvector tsquery ──> PostgreSQL FTS         ──> fts_results     (N×2 candidates)
                             (language-aware stemming)
                                         │
                                         ▼
                               RRF merge:
                               score(doc) = 1/(60 + rank_sem + 1)
                                          + 1/(60 + rank_fts + 1)
                                         │
                                         ▼
                               dedup_by_parent(): keep best chunk per document
                                         │
                                         ▼
                               MMR reranking (semantic path only)
```

**RRF formula:** `score = 1/(K + rank_sem + 1) + 1/(K + rank_fts + 1)` where `K = 60`.

If the embedding endpoint is unavailable at query time, the search degrades gracefully to FTS-only (mode returned as `"fts"`). The search mode string is included in tool output.

### MMR Reranking

Applied to semantic candidates before RRF merge. Parameters: `lambda = 0.75`, `candidateMultiplier = 6` (fetches 6× the requested limit as candidates).

```
selected = []
for i in 0..limit:
    best = argmax over candidates:
        score = λ × (similarity × relevance_score)
              - (1 - λ) × max(similarity_to_any_selected)
    selected.append(best)
    candidates.remove(best)
```

The inter-result similarity is approximated as `min(candidate_sim, selected_sim)` since cross-embeddings are not computed.

### Two-Tier Memory

| Tier | `pinned` field | Behavior |
|------|---------------|----------|
| Raw | `false` | Subject to temporal decay (daily cron at 03:00 UTC): `relevance_score` multiplied by `exp(-0.693 / 30 * days_since_accessed)` (exponential decay with 30-day half-life). A second cleanup job at 08:00 UTC deletes chunks where `relevance_score < 0.1` AND `accessed_at` is older than 180 days. |
| Pinned | `true` | Never decayed, never deleted by the decay job. Permanent facts explicitly stored by the agent. |

Both tiers are searched together. The `pinned` flag is surfaced in search results so the LLM can distinguish permanent knowledge from time-sensitive context.

### Memory Worker

`hydeclaw-memory-worker` is a separate binary that handles asynchronous memory indexing tasks enqueued by the core. It uses a poll-based task queue in PostgreSQL:

```
claim_next() → UPDATE tasks SET status='processing' WHERE status='pending' ... RETURNING *
    │
    ▼
handlers::dispatch(task) → embed text → insert memory_chunks with pgvector embedding
    │
    ▼
mark_done() or mark_failed()
```

The worker runs as a single-threaded tokio runtime (`current_thread`) with a DB pool of 3 connections. It sends sd-notify `READY=1` on Linux for systemd integration. Stuck `processing` tasks from a previous crash are recovered at startup.

---

## Secrets Vault

Secrets are encrypted at rest in the `secrets` PostgreSQL table using **ChaCha20-Poly1305** (authenticated encryption).

```
┌───────────────────────────────────────────────────┐
│                    secrets table                  │
│  PK: (name, scope)                               │
│  encrypted_value: bytea  (ciphertext + auth tag) │
│  nonce: bytea            (12 bytes, random)       │
└───────────────────────────────────────────────────┘
         │ decrypt at startup with HYDECLAW_MASTER_KEY (32-byte hex)
         ▼
┌─────────────────────────────────────────────────────┐
│       SecretsManager in-memory cache                │
│       RwLock<HashMap<(name, scope), String>>        │
└─────────────────────────────────────────────────────┘
```

Each secret uses a **unique 12-byte random nonce** generated at write time, so two writes of the same value produce different ciphertexts. The master key is never stored in the DB.

### Scoping Resolution

`get_scoped(name, scope)` applies a three-level fallback:

```
1. (name, scope)   — agent-specific secret (scope = agent name, e.g. "main")
2. (name, "")      — global secret (scope = empty string)
3. env::var(name)  — environment variable fallback (for migration convenience)
```

This allows, for example, `BOT_TOKEN` to be stored globally while `BOT_TOKEN` with `scope="analyst"` overrides it for that specific agent. Agent renaming automatically migrates scoped secrets via `rename_scope(old, new)`.

Channel adapter credentials (bot tokens) are extracted from agent config on first startup and stored as scoped secrets keyed by channel UUID, then removed from the config file.

---

## Channel System

### WebSocket Loopback Architecture

Channel adapters (Telegram, Discord, etc.) connect to Core via a WebSocket handshake at `GET /ws/channel/{agent_name}`. This creates a bidirectional loopback within the same process:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         hydeclaw-core                                   │
│                                                                         │
│   channel adapter task (tokio)          AgentEngine                    │
│   ┌─────────────────────────────────┐   ┌────────────────────────────┐  │
│   │  grammy / discord.js / etc.     │   │                            │  │
│   │  listens on external API        │   │  tool handlers             │  │
│   │          │                      │   │       │                    │  │
│   │          ▼                      │   │       ▼                    │  │
│   │  ChannelInbound (JSON)  ────────┼──>│  IncomingMessage           │  │
│   │                                 │   │  (via WS loopback)         │  │
│   │  ChannelOutbound / Action <─────┼───│  ChannelAction             │  │
│   │  (react, send_voice, etc.)      │   │  (via ChannelActionRouter) │  │
│   └─────────────────────────────────┘   └────────────────────────────┘  │
│                  ▲                                                       │
│                  │ WS frames (JSON)                                      │
│         /ws/channel/{agent_name}                                        │
└─────────────────────────────────────────────────────────────────────────┘
```

The adapter and engine never share memory directly — all communication is serialized JSON over the internal WebSocket, making channel adapters fully replaceable and independently restartable.

### Inbound Message Flow

```
1. Adapter receives external message (e.g. Telegram update via grammy long-poll)
2. Adapter serializes to ChannelInbound JSON: {text, attachments, context, sender_id, ...}
3. Adapter sends JSON frame over /ws/channel/{agent_name}
4. Gateway WS handler deserializes to IncomingMessage
   └── context: opaque serde_json::Value (Immutable Core principle — engine never inspects it)
5. AccessGuard.is_allowed(sender_id) check
6. engine.handle_with_status() called in spawned task
7. Response text sent back as ChannelOutbound frame
8. Adapter formats and delivers response to the user
```

### Outbound Action Flow

When the engine executes a tool that produces a channel action (e.g., `send_voice`, `react`, `pin`):

```
1. YAML tool executes HTTP call → receives binary payload (audio bytes, etc.)
2. channel_action config in YAML tool triggers ChannelActionRouter.send()
3. ChannelActionRouter looks up adapter by channel type ("telegram", "discord", etc.)
4. Sends ChannelAction {name, params, context} via bounded mpsc channel (capacity 64)
5. Adapter's action receiver loop picks it up
6. Adapter performs the platform-specific action (uploads file, sets reaction, etc.)
7. Result sent back via oneshot::Sender<Result<(), String>>
```

### ChannelActionRouter

Maintains a `RwLock<HashMap<String, mpsc::Sender<ChannelAction>>>` keyed by `"{channel_type}:{uuid}"`. The UUID is generated fresh on each WebSocket connection, preventing stale senders from a previous connection from being used after reconnection.

When an engine action specifies a `target_channel`, the router finds the first registered sender with a matching prefix. If no target is specified, the first available channel is used.

### Access Guard

`AccessGuard` enforces per-agent access control:

- `restricted: false` — all users allowed (default for personal agents).
- `restricted: true` — only the `owner_id` and users in the `access_list` DB table can interact.
- `owner_id` check is a fast string comparison (no DB query).
- Non-owner access is checked via `access::is_user_allowed(&db, agent_id, channel_user_id)`.
- Pairing codes: the owner can generate time-limited codes to grant access to other users.

---

## Tool System

### Three Tool Types

```
IncomingMessage.tool_calls[]
         │
         ▼
execute_tool_call(name, args)
         │
         ├── System tools ──────────────── handled in engine (Rust code)
         │   (workspace_*, memory_*, agent, etc.)
         │
         ├── YAML tools ─────────────────── loaded from workspace/tools/*.yaml
         │   (web APIs, external HTTP services)
         │
         └── MCP tools ──────────────────── Docker containers via bollard
             (name prefixed with mcp: or resolved via McpRegistry)
```

### Tool Policy

The deny list is checked first, before any execution:

```
agent config: tools.deny = ["tool_name", ...]
                              │
                              ▼ (checked first, before any other routing)
                         return "tool is denied"
```

Draft YAML tools (status: `draft`) are excluded from the tool list shown to the LLM unless `include_draft = true` is set in config. Disabled tools (`status: disabled`) are never loaded.

### YAML Tool Execution

Each YAML file in `workspace/tools/` defines one tool. The engine loads them with a 30-second in-memory cache to avoid per-batch disk reads during parallel execution.

Execution flow:

```
1. Resolve parameters:
   ├── LLM-provided values (from tool call arguments)
   ├── default_from_env: resolve via SecretsEnvResolver (agent-scoped → global → env fallback)
   └── default: literal fallback value

2. Build HTTP request:
   ├── path parameters: {param} substitution in URL template
   ├── query parameters: appended to URL
   ├── header parameters: added to request headers
   └── body parameters: JSON body (or form-encoded)

3. Auth:
   ├── bearer_env:      Authorization: Bearer {resolve_env(key)}
   ├── basic_env:       Authorization: Basic base64({username}:{password})
   ├── api_key_header:  {header_name}: {resolve_env(key)}
   ├── api_key_query:   ?{param_name}={resolve_env(key)}
   ├── custom:          arbitrary headers with ${VAR} substitution
   ├── oauth_refresh:   POST to token_url with refresh token → cache access token
   ├── oauth_provider:  OAuth 2.0 via OAuthManager (PKCE flow)
   └── none:            no auth

4. Execute HTTP call (standard reqwest client — NOT SSRF-safe, tools are admin-configured)

5. response_transform: optional JSONPath extraction ("$.ok", "$.web.results")

6. If channel_action configured: route binary result to ChannelActionRouter
   instead of returning text to LLM (used for TTS audio, images, etc.)

7. Return text result to LLM context
```

### SSRF Protection

User-supplied URLs (in `web_fetch`, `fetch_url_content`) use a dedicated `ssrf_http_client` built with `SsrfSafeResolver`:

**Layer 1 — Sync pre-check** (`validate_url_scheme()`):
- Only `http://` and `https://` schemes allowed.
- Internal service blocklist checked by `host:port` (toolgate, PostgreSQL, Docker socket, SearXNG, browser-renderer, core itself).
- Numeric private IPs in URL blocked immediately (no DNS needed).

**Layer 2 — DNS-time filtering** (`SsrfSafeResolver`):
- Custom `reqwest::dns::Resolve` implementation.
- After DNS resolution, filters out all private/internal IPs:
  - IPv4: loopback (127/8), RFC 1918, link-local (169.254/16), CGNAT (100.64/10), broadcast.
  - IPv6: loopback (::1), unique-local (fc00::/7), link-local (fe80::/10).
- If all resolved addresses are private, returns `PermissionDenied` — connection never attempted.
- Eliminates the DNS rebinding TOCTOU gap between validation and connection.

YAML tools and Toolgate calls bypass SSRF checks entirely (admin-configured endpoints).

### MCP (Model Context Protocol)

MCP servers run as Docker containers managed via `bollard`. The `McpRegistry` wraps a `ContainerManager`:

```
LLM requests mcp tool "mcp_name__tool_name"
         │
         ▼
McpRegistry.call_tool("mcp_name", "tool_name", args)
         │
         ├── ContainerManager.ensure_running("mcp_name")
         │   ├── check if container already running
         │   ├── if not: docker pull + docker run (via bollard async API)
         │   └── return base_url (e.g. "http://container-ip:8080")
         │
         └── POST {base_url}/mcp
             Content-Type: application/json
             Accept: application/json, text/event-stream
             {
               "jsonrpc": "2.0",
               "method": "tools/call",
               "params": {"name": "tool_name", "arguments": args},
               "id": 2
             }
             │
             └── parse JSON-RPC 2.0 response → tool result string
```

Tool definitions are discovered via `tools/list` and cached in `tool_cache: RwLock<HashMap<String, Vec<ToolDefinition>>>`. The cache is invalidated when a container is restarted.

---

## Scheduler

### Heartbeat Mechanism

Each agent can configure a heartbeat — a cron-scheduled synthetic message sent to itself:

```toml
[agent.heartbeat]
cron = "*/30 10-19 * * *"    # Every 30 min, 10:00–19:00 local time
timezone = "UTC"
announce_to = "telegram"      # Channel to deliver non-OK responses
```

Execution flow:

```
1. tokio-cron-scheduler fires at UTC-converted time
2. Per-agent lock check: if agent already running a scheduled task, skip this tick
3. Acquire agent lock (HashSet<String>)
4. Construct synthetic IncomingMessage {text: heartbeat_task_message, channel: "heartbeat"}
5. engine.handle_isolated() → LLM processes the heartbeat task
6. Check response:
   ├── contains "HEARTBEAT_OK" → log info, silent (no delivery)
   └── any other response → deliver to announce_to channel via ChannelActionRouter
7. Release agent lock
```

**Cron normalization:** standard 5-field cron is converted to 6-field (prepend `"0 "` for seconds). Local timezone hours are converted to UTC by computing effective UTC offsets for each hour in the cron expression.

### Dynamic Cron Jobs

Agents can create, modify, and delete their own scheduled jobs via the `cron_add` / `cron_delete` tools. Jobs are stored in the `scheduled_jobs` PostgreSQL table and hot-loaded into the running scheduler without restart.

Supported options per job: `cron_expr`, `timezone`, `task_message`, `silent`, `announce_to`, `jitter_secs` (randomize execution time within a window), `run_once` (auto-delete after first execution), `run_at` (one-shot future time).

### Per-Agent Execution Lock

A single `AgentLocks: Arc<Mutex<HashSet<String>>>` is shared across all scheduled jobs for all agents. Before executing any heartbeat or dynamic cron task, the agent name is inserted into the set. On completion (or error), it is removed. If the agent name is already present, the tick is skipped. This prevents concurrent scheduled executions from the same agent accumulating and starving the tokio runtime.

---

## Process Manager

`ProcessManager` manages long-lived child processes (channel adapters, toolgate, browser-renderer) spawned by Core at startup.

### Spawn

```rust
tokio::process::Command::new(exe)
    .args(args)
    .current_dir(working_dir)
    .envs(env)               // selected passthrough + injected vars
    .process_group(0)        // own process group → SIGKILL kills grandchildren too
    .spawn()
```

Process groups ensure that multi-process children (e.g., uvicorn with worker subprocesses) are fully terminated when Core kills the parent.

Environment is minimal by design: only explicitly listed `env_passthrough` keys are forwarded, plus any `env_extra` literals (with `${VAR}` substitution support). DB credentials are never forwarded to channel adapters.

### Monitor Loop

A background `tokio::spawn` polls each child's `try_wait()` every 5 seconds. If a child has exited unexpectedly, it is respawned with exponential backoff (restart count tracked per process). The `POST /api/services/{name}/restart` endpoint kills and respawns a named process on demand.

### Graceful Shutdown

On `SIGTERM` / `Ctrl-C`:

```
1. tokio::signal::unix::signal(SIGTERM) fires
2. ProcessManager.kill(name) for each managed process:
   a. child.kill() (SIGKILL to the process group)
   b. tokio::time::timeout(3s, child.wait())
3. DB pool closes
4. Core exits
```

---

## UI Architecture

### Stack

| Layer | Technology |
|-------|-----------|
| Framework | Next.js 16 (App Router) + React 19 |
| State management | Zustand + Immer |
| UI components | shadcn/ui (Radix primitives + Tailwind) |
| Data fetching | TanStack Query v5 (with persist client) |
| Rich text editor | TipTap v3 (workspace markdown editor) |
| Code editor | CodeMirror 6 |
| Graph visualization | Sigma.js (react-sigma) + Dagre layout |
| Chat rendering | Custom multi-agent renderer (no assistant-ui) |
| Internationalization | Custom i18n (en/ru) |

### Chat Store

`chat-store.ts` (Zustand + Immer) is the central state machine for the chat UI. It maintains per-agent state:

```typescript
interface AgentState {
  sessions: SessionRow[];          // paginated list
  messages: UIMessage[];           // current session, Immer-mutable
  currentSessionId: string | null;
  isStreaming: boolean;
  streamController: AbortController | null;
}
```

**SSE parsing:** `sendMessage()` opens a `fetch()` to `POST /api/chat` with `Accept: text/event-stream`. Each line is parsed by `parseSSELines()` + `parseSseEvent()` into a discriminated `SseEvent` union. Events are applied to `messages[]` via Immer mutations:

- `text-delta` → appends to the last `TextPart` in the current assistant message.
- `tool-input-start` → adds a `ToolPart` with `state: "input-streaming"`.
- `tool-input-available` → transitions to `state: "input-available"`, sets `input`.
- `tool-output-available` → transitions to `state: "output-available"`, sets `output`.
- `rich-card` → appends a `RichCardPart` with structured data for table/metric rendering.
- `file` → appends a `FilePart` with URL + media type for inline image/audio display.
- `finish` → sets `isStreaming: false`, releases the AbortController.

### WebSocket Event Bus

`ws-store.ts` (Zustand) manages a single persistent WebSocket connection to `GET /ws`. The `WsManager` class handles:

- Reconnection with exponential backoff.
- One-time ticket auth (`POST /api/auth/ws-ticket` → token valid for one WS connection).
- Broadcast of typed events to subscribers via a simple listener map.

WebSocket events from Core include: `agent_processing` (start/end — drives global loading indicators), `session_updated` (cache invalidation), `approval_requested` (tool approval modal), `log_entry` (live log feed).

Subscribers register via `useWsSubscription(eventType, handler)` hook and are automatically cleaned up on component unmount.

### Static Export with RSC Flattening

The UI is built as a static export (`next build` → `out/`) for deployment as a Docker nginx container. Because Next.js App Router emits React Server Component payloads (`.rsc` files) alongside HTML, a post-build script `scripts/flatten-rsc.mjs` processes the output:

- Replaces RSC `<script>` injection with direct HTML so the static files are self-contained.
- Ensures the nginx container can serve the app from any path without a Node.js runtime.

The `out/` directory and an `nginx.conf` are the only artifacts deployed to the Pi — no source code, no Node.js, no build tools on the target host.

---

## Setup Wizard

The Setup Wizard guides first-time users through instance configuration. It runs when `GET /api/setup/status` reports `setup_complete: false` (no providers configured, no agents created).

```
Browser opens http://host:18789
         │
         ▼
GET /api/setup/status  →  { setup_complete: false }
         │
         ▼
UI redirects to /setup
         │
         ├── Step 1: Prerequisites check (GET /api/setup/requirements)
         │   ├── database connectivity
         │   ├── master key present
         │   └── Docker availability
         │
         ├── Step 2: Provider configuration
         │   └── user enters API key + selects model
         │
         ├── Step 3: Create initial agent
         │   └── name, provider, model, basic settings
         │
         └── Step 4: POST /api/setup/complete
             └── marks instance as configured
```

After setup completes, the wizard is bypassed on all subsequent visits. The setup state is persisted in the database, not in a file.

---

## Network Discovery

Core detects all non-loopback network interfaces at startup and exposes them via `GET /api/network/addresses`. This allows the UI to display clickable access URLs (e.g., `http://192.168.1.85:18789`) during setup and on the settings page.

Detection uses platform-native APIs (`getifaddrs` on Linux, equivalent on other platforms). Both IPv4 and IPv6 addresses are returned, tagged with interface name and address family. The result is cached and refreshed on each API call (interfaces can change due to DHCP or VPN).

---

## Notifications System

In-app notifications provide a persistent feed of system events that require user attention. Notifications are stored in PostgreSQL and delivered to the UI via the existing WebSocket event bus.

```
Event source (engine, watchdog, scheduler)
         │
         ▼
  notification_create(type, title, body)
         │
         ├── INSERT into notifications table
         └── broadcast via WS: { event: "notification", ... }
                   │
                   ▼
         UI NotificationBell component
         (badge count, dropdown list, mark-read actions)
```

**Notification types:** `agent_error` (LLM or tool failure), `watchdog_alert` (health check failure), `setup_required` (missing configuration), `update_available` (new version detected), `approval_needed` (pending tool approval).

Lifecycle: notifications are created as unread, can be individually marked read via `PATCH /api/notifications/{id}`, bulk-marked via `POST /api/notifications/read-all`, and cleared (read only) via `DELETE /api/notifications/clear`.

---

## Base Agent Scaffold

When a new base agent is created (via API or setup wizard), the scaffold system bootstraps it with a default directory structure under `workspace/`:

```
workspace/
  └── agents/{agent-name}/
      ├── SOUL.md          # personality and behavioral directives
      ├── IDENTITY.md      # name, role, capabilities summary
      └── skills/          # agent-specific skill files (initially empty)
```

The scaffold templates are embedded in the binary (not read from disk at runtime). `SOUL.md` and `IDENTITY.md` are created with sensible defaults that can be customized later. For base agents, these files are read-only via the workspace write protection in `workspace.rs:is_read_only()` -- only manual edits on disk or admin API calls can modify them.

Non-base agents do not receive a scaffold; their workspace directories are created on first write.
