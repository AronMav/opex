# HydeClaw API Reference

**Base URL:** `http://<host>:18789`

**Authentication:** All routes require `Authorization: Bearer <HYDECLAW_AUTH_TOKEN>` unless explicitly marked **Public**. The token is configured via `gateway.auth_token_env` in `hydeclaw.toml`.

**Rate limiting:** 500 failed auth attempts per IP triggers a 30-second lockout. General request rate limiting is configurable via `limits.max_requests_per_minute`. Loopback and authenticated requests are exempt.

---

## Table of Contents

1. [Authentication](#1-authentication)
2. [Monitoring & Health](#2-monitoring--health)
3. [Agents](#3-agents)
4. [Chat — OpenAI-Compatible](#4-chat--openai-compatible)
5. [Chat SSE (Native Streaming)](#5-chat-sse-native-streaming)
6. [Sessions and Messages](#6-sessions-and-messages)
7. [Memory](#7-memory)
8. [Tools and MCP](#8-tools-and-mcp)
9. [YAML Tools](#9-yaml-tools)
10. [Skills](#10-skills)
11. [Channels](#11-channels)
12. [Cron Jobs](#12-cron-jobs)
13. [Tasks](#13-tasks)
14. [Approvals](#14-approvals)
15. [Webhooks](#15-webhooks)
16. [Secrets](#16-secrets)
17. [Config](#17-config)
18. [Backup and Restore](#18-backup-and-restore)
19. [Services](#19-services)
20. [Watchdog](#20-watchdog)
21. [Providers](#21-providers)
22. [TTS and Canvas](#22-tts-and-canvas)
23. [Media Upload](#23-media-upload)
24. [Workspace](#24-workspace)
25. [Workspace Files (Signed URLs)](#25-workspace-files-signed-urls)
26. [OAuth](#26-oauth)
27. [Access / Pairing](#27-access--pairing)
28. [WebSocket (UI Events)](#28-websocket-ui-events)
29. [Email Triggers (Gmail)](#29-email-triggers-gmail)
30. [GitHub Integration](#30-github-integration)
31. [Setup](#31-setup)
32. [Network](#32-network)
33. [Notifications](#33-notifications)
34. [Curator](#34-curator)
35. [Session Failures](#35-session-failures)
36. [CSP Reports](#36-csp-reports)

---

## 1. Authentication

### WS Ticket

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/api/auth/ws-ticket` | Required | Issue a one-time WebSocket ticket |

The WebSocket endpoint (`/ws`) requires authentication. Use this endpoint to obtain a short-lived ticket and pass it as `?ticket=<uuid>`.

**Response:**
```json
{ "ticket": "uuid-v4-string" }
```

Tickets are valid for **30 seconds** and consumed on first use.

---

## 2. Monitoring & Health

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/health` | Public | Liveness check — returns `200 OK`, no body |
| `GET` | `/api/status` | Required | Full gateway status |
| `GET` | `/api/stats` | Required | Message/session statistics |
| `GET` | `/api/usage` | Required | Token usage summary |
| `GET` | `/api/usage/daily` | Required | Daily token usage breakdown |
| `GET` | `/api/usage/sessions` | Required | Per-session token usage |
| `GET` | `/api/doctor` | Required | Deep health check of all subsystems |
| `GET` | `/api/health/dashboard` | Required | Runtime metrics dashboard |
| `GET` | `/api/audit` | Required | Audit event log |
| `GET` | `/api/audit/tools` | Required | Tool invocation audit log |

### GET /api/status

**Response:**
```json
{
  "status": "ok",
  "version": "0.x.x",
  "uptime_seconds": 12345,
  "db": true,
  "listen": "0.0.0.0:18789",
  "agents": ["main", "analyst"],
  "memory_chunks": 2901,
  "scheduled_jobs": 3,
  "active_sessions": 5,
  "tools_registered": 12
}
```

### GET /api/stats

**Response:**
```json
{
  "messages_today": 42,
  "sessions_today": 5,
  "total_messages": 18000,
  "total_sessions": 592,
  "recent_sessions": [
    {
      "id": "uuid",
      "agent_id": "main",
      "channel": "ui",
      "last_message_at": "2026-03-27T10:00:00Z",
      "title": "Session title"
    }
  ]
}
```

### GET /api/usage

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `days` | integer | 30 | Lookback window |
| `agent` | string | — | Filter by agent name |

### GET /api/doctor

Returns health status of all subsystems with latency measurements.

**Response:**
```json
{
  "ok": true,
  "checks": {
    "database": { "status": "ok", "latency_ms": 2, "message": "..." },
    "toolgate":  { "status": "ok", "latency_ms": 15, "message": "..." },
    "secrets":   { "status": "ok", "message": "..." },
    "channels":  { "status": "ok", "latency_ms": 3, "message": "..." }
  }
}
```

Each check has `status` (`"ok"`, `"warn"`, or `"error"`), `message`, optional `latency_ms`, `fix_hint`, and `details`.

### GET /api/health/dashboard

Returns runtime counters and pool sizes. Unknown fields are opaque — clients must not assume the field set is stable.

**Response:**
```json
{
  "version": "0.x.x",
  "sse_events_dropped_total": { "<agent>": { "<event_type>": 0 } },
  "csp_violations": {},
  "csp_violations_overflow": 0,
  "active_agents": 2,
  "sse_streams": 1,
  "approval_waiters": 0,
  "auth_rate_limiter_size": 0,
  "request_rate_limiter_size": 0,
  "stream_registry_size": 1,
  "db_pool_total": 5,
  "db_pool_idle": 4,
  "memory_worker_heartbeat_age_secs": 120,
  "session_timeline_table_size_bytes": 204800,
  "uptime_secs": 3600
}
```

### GET /api/audit

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `agent` | string | — | Filter by agent name |
| `event_type` | string | — | Filter by event type |
| `limit` | integer | 100 | Max results (max 500) |
| `offset` | integer | 0 | Pagination offset |

### GET /api/audit/tools

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `agent` | string | — | Filter by agent |
| `tool` | string | — | Filter by tool name |
| `days` | integer | 7 | Lookback window |
| `limit` | integer | 100 | Max results (max 500) |

---

## 3. Agents

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/agents` | List all agents (base-first, then alphabetical) |
| `POST` | `/api/agents` | Create a new agent |
| `GET` | `/api/agents/{name}` | Get agent details |
| `PUT` | `/api/agents/{name}` | Update agent config |
| `DELETE` | `/api/agents/{name}` | Delete agent |
| `POST` | `/api/agents/{name}/model-override` | Temporarily override LLM model in-memory |
| `GET` | `/api/agents/{name}/tasks` | List tasks for an agent |
| `GET` | `/api/agents/{name}/hooks` | Get agent hook configuration |

### GET /api/agents

**Response:**
```json
{
  "agents": [
    {
      "name": "main",
      "language": "ru",
      "model": "MiniMax-M2.5",
      "provider": "minimax",
      "icon": "🤖",
      "temperature": 1.0,
      "has_access": true,
      "access_mode": "allowlist",
      "has_heartbeat": false,
      "heartbeat_cron": null,
      "heartbeat_timezone": null,
      "tool_policy": { "allow": [], "deny": [], "allow_all": true },
      "routing_count": 0,
      "is_running": true,
      "config_dirty": false,
      "base": true
    }
  ]
}
```

### POST /api/agents

Create a new agent. Config is written to `config/agents/{name}.toml` and the agent starts immediately. The first agent created automatically gets `base = true` with restricted access defaults.

**Request body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Agent name (alphanumeric, `-`, `_`, max 32 chars) |
| `provider` | string | Yes | LLM provider type (e.g. `minimax`, `openai`, `anthropic`) |
| `model` | string | Yes | Model identifier |
| `provider_connection` | string | No | Named LLM provider connection ID (overrides provider/model) |
| `language` | string | No | Response language hint |
| `temperature` | float | No | Sampling temperature |
| `max_tokens` | integer | No | Max response tokens |
| `icon` | string | No | Emoji or icon string |
| `voice` | string | No | TTS voice name (stored as scoped secret `TTS_VOICE`) |
| `access` | object\|null | No | Access control config |
| `heartbeat` | object\|null | No | Heartbeat cron config |
| `tools` | object\|null | No | Tool policy |
| `compaction` | object\|null | No | Context compaction config |
| `session` | object\|null | No | Session management config |
| `routing` | array\|null | No | LLM routing rules |
| `approval` | object\|null | No | Human approval config |
| `tool_loop` | object\|null | No | Tool loop config |
| `max_tools_in_context` | integer | No | Max tool definitions injected into context |
| `max_history_messages` | integer | No | Max messages loaded from session history |
| `max_agent_turns` | integer | No | Max turns before automatic stop |
| `daily_budget_tokens` | integer | No | Daily token budget cap (0 = no limit) |

**`access` object:**

| Field | Type | Description |
|-------|------|-------------|
| `mode` | string | `"allowlist"`, `"open"`, or `"restricted"` |
| `owner_id` | string | Channel user ID with admin rights |

**`heartbeat` object:**

| Field | Type | Description |
|-------|------|-------------|
| `cron` | string | Cron expression |
| `timezone` | string | IANA timezone (default `"UTC"`) |
| `announce_to` | string | Channel name to post heartbeat messages to |

**`tools` object:**

| Field | Type | Description |
|-------|------|-------------|
| `allow` | array | Explicitly allowed tool names |
| `deny` | array | Explicitly denied tool names |
| `allow_all` | bool | If true, all tools allowed by default |
| `deny_all_others` | bool | If true, only `allow` list permitted |
| `groups.git` | bool | Enable git tool group |
| `groups.tool_management` | bool | Enable tool management tools |
| `groups.skill_editing` | bool | Enable skill editing tools |
| `groups.session_tools` | bool | Enable session management tools |

**`compaction` object:**

| Field | Type | Description |
|-------|------|-------------|
| `enabled` | bool | Enable automatic context compaction |
| `threshold` | float | Token fraction threshold that triggers compaction |
| `preserve_tool_calls` | bool | Keep tool call/result pairs in summary |
| `preserve_last_n` | integer | Always preserve the last N messages |
| `max_context_tokens` | integer | Hard limit before emergency compaction |

**`approval` object:**

| Field | Type | Description |
|-------|------|-------------|
| `enabled` | bool | Enable human-in-the-loop approval |
| `require_for` | array | Tool names that require approval |
| `require_for_categories` | array | Tool categories requiring approval |
| `timeout_seconds` | integer | Seconds to wait before auto-deny |

**`tool_loop` object:**

| Field | Type | Description |
|-------|------|-------------|
| `max_iterations` | integer | Max total tool calls per session turn |
| `compact_on_overflow` | bool | Trigger compaction when iterations exceeded |
| `detect_loops` | bool | Enable n-gram loop detection |
| `warn_threshold` | integer | Iteration count for loop warning |
| `break_threshold` | integer | Iteration count to force-break loop |
| `max_consecutive_failures` | integer | Consecutive tool failures before breaking |
| `max_auto_continues` | integer | Max automatic continue prompts |
| `max_loop_nudges` | integer | Max loop-nudge messages injected |
| `ngram_cycle_length` | integer | N-gram window for cycle detection |

**`session` object:**

| Field | Type | Description |
|-------|------|-------------|
| `dm_scope` | string | DM session scope identifier |
| `ttl_days` | integer | Days before idle sessions expire |
| `max_messages` | integer | Max messages before auto-compaction |
| `prune_tool_output_after_turns` | integer | Remove tool output from context after N turns |

**`skill_review` object:**

| Field | Type | Description |
|-------|------|-------------|
| `enabled` | bool | Enable skill review after sessions |
| `min_tool_calls` | integer | Minimum tool calls to trigger review |

**`hooks` object:**

| Field | Type | Description |
|-------|------|-------------|
| `log_all_tool_calls` | bool | Log every tool call to audit |
| `block_tools` | array | Tool names to block (alternative to deny list) |

**Response:** `{ "ok": true, "name": "agent-name" }`

### GET /api/agents/{name}

Returns full `AgentDetailDto`. The `config_dirty` flag is `true` when the running config diverges from the on-disk config.

### PUT /api/agents/{name}

Update agent config. Accepts the same fields as `POST /api/agents`. Field merge semantics:
- **Field absent in payload**: existing value is preserved.
- **Explicit `null`**: value is cleared.
- **Value provided**: value is updated.

`base` and `base` flags are **never** changed via PUT — preserved from disk.
Renaming an agent (via `name` field in payload) updates 21 DB tables in a transaction, renames the workspace directory, and migrates scoped secrets. Base agents cannot be renamed.

**Response:** `{ "ok": true, "name": "new-name" }`

### DELETE /api/agents/{name}

Stops and removes the agent. Config file is deleted. Returns `{ "ok": true }`.

### POST /api/agents/{name}/model-override

Temporarily override LLM model for a running agent (in-memory, lost on restart).

**Request body:**
```json
{ "model": "gpt-4o", "provider": "openai" }
```

---

## 4. Chat — OpenAI-Compatible

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/v1/chat/completions` | Required | OpenAI-compatible chat completions |
| `GET` | `/v1/models` | Required | List available models |
| `POST` | `/v1/embeddings` | Required | Proxy embeddings request to Toolgate |

### POST /v1/chat/completions

OpenAI-format chat completions. Supports streaming (`"stream": true`) and non-streaming modes.

**Request body:**

| Field | Type | Description |
|-------|------|-------------|
| `messages` | array | OpenAI-format message array |
| `model` | string | Model name (informational) |
| `temperature` | float | Sampling temperature |
| `stream` | bool | Enable SSE streaming (default: false) |
| `agent` | string | HydeClaw extension: target agent name |

---

## 5. Chat SSE (Native Streaming)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/chat` | Start a streaming chat session (SSE) |
| `GET` | `/api/chat/{id}/stream` | Resume a stream by stream ID |
| `POST` | `/api/chat/{id}/abort` | Abort an in-progress stream |

### POST /api/chat

Primary chat endpoint. Returns Server-Sent Events compatible with the **Vercel AI SDK v3** (`useChat` hook).

**Request body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `agent` | string | Yes | Target agent name |
| `message` | string | Yes | User message text |
| `session_id` | string (UUID) | No | Continue an existing session |
| `channel` | string | No | Source channel identifier (default: `"ui"`) |
| `user_id` | string | No | User identifier for access control |
| `attachments` | array | No | File attachments |
| `leaf_message_id` | UUID | No | Resume from a specific branch leaf |
| `user_message_id` | UUID | No | Explicit message ID for idempotency |
| `tool_policy_override` | object | No | Per-request tool policy override |
| `formatting_prompt` | string | No | Additional formatting instruction |

**Attachment object:**

| Field | Type | Description |
|-------|------|-------------|
| `url` | string | Public URL of the attachment |
| `content_type` | string | MIME type (e.g. `image/jpeg`, `audio/ogg`) |
| `filename` | string | Original filename |

**SSE event types:**

| Event type | Description | Key payload fields |
|------------|-------------|-------------------|
| `data-session-id` | First event; contains the session ID | `{ "sessionId": "uuid" }` |
| `start` | Stream begins | `{ "session_id": "uuid", "stream_id": "uuid" }` |
| `text-start` | Text block starting | `{ "id": "block-uuid" }` |
| `text-delta` | Incremental text chunk | `{ "delta": "text" }` |
| `text-end` | Text block complete | `{}` |
| `tool-input-start` | Tool call starting | `{ "toolCallId": "id", "toolName": "search" }` |
| `tool-input-delta` | Tool arguments streaming | `{ "toolCallId": "id", "inputTextDelta": "{\"q\":" }` |
| `tool-input-available` | Full tool call ready | `{ "toolCallId": "id", "toolName": "search", "input": {...} }` |
| `tool-output-available` | Tool result ready | `{ "toolCallId": "id", "output": "..." }` |
| `rich-card` | Structured display card | `{ "cardType": "...", "data": {...} }` |
| `file` | File produced by tool | `{ "url": "...", "mediaType": "audio/ogg" }` |
| `sync` | Message sync | `{ "content": "...", "toolCalls": [...], "status": "...", "error": null }` |
| `tool-approval-needed` | Tool awaiting human approval | `{ "approvalId": "uuid", "toolName": "...", "args": {...} }` |
| `tool-approval-resolved` | Approval decision made | `{ "approvalId": "uuid", "decision": "approve" }` |
| `reconnecting` | Stream reconnect in progress | `{}` |
| `usage` | Token usage update | `{ "input_tokens": 100, "output_tokens": 50 }` |
| `finish` | Stream complete | `{ "usage": {...}, "tools_used": [...] }` |
| `error` | Error during processing | `{ "errorText": "error text" }` |

### GET /api/chat/{id}/stream

Resume a previously started stream by its `stream_id`. Returns same SSE format.

### POST /api/chat/{id}/abort

Abort an in-progress stream. Agent stops processing.

**Response:** `{ "ok": true }` or `{ "ok": false, "error": "stream not found" }`

---

## 6. Sessions and Messages

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/sessions` | List sessions (requires `?agent=`) |
| `DELETE` | `/api/sessions` | Delete all sessions (requires `?agent=` or `?channel=`) |
| `GET` | `/api/sessions/latest` | Get latest session for an agent (requires `?agent=`) |
| `GET` | `/api/sessions/search` | Full-text search across messages |
| `GET` | `/api/sessions/stuck` | Find stuck/stale sessions for retry |
| `GET` | `/api/sessions/failures` | Paginated session failure log |
| `GET` | `/api/sessions/{id}` | Get session metadata |
| `PATCH` | `/api/sessions/{id}` | Update session title or UI state (requires `?agent=`) |
| `DELETE` | `/api/sessions/{id}` | Delete session and all messages (requires `?agent=`) |
| `POST` | `/api/sessions/{id}/compact` | Manually compact session history (requires `?agent=`) |
| `GET` | `/api/sessions/{id}/export` | Export session as JSON or Markdown (requires `?agent=`) |
| `POST` | `/api/sessions/{id}/invite` | Invite an agent into a multi-agent session |
| `POST` | `/api/sessions/{id}/fork` | Create a branched message from an existing message (requires `?agent=`) |
| `GET` | `/api/sessions/{id}/active-path` | Get the active message branch path (requires `?agent=`) |
| `GET` | `/api/sessions/{id}/chain` | Get the full session chain (parent + child sessions) (requires `?agent=`) |
| `POST` | `/api/sessions/{id}/retry` | Replay last user message through engine (requires `?agent=`) |
| `GET` | `/api/sessions/{id}/messages` | List messages in a session (requires `?agent=`) |
| `GET` | `/api/sessions/{id}/failures` | Get failure records for one session |
| `DELETE` | `/api/messages/{id}` | Delete a single message (requires `?agent=`) |
| `PATCH` | `/api/messages/{id}` | Edit a user message (requires `?agent=`) |
| `POST` | `/api/messages/{id}/feedback` | Set message feedback (requires `?agent=`) |

### GET /api/sessions

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `agent` | string | Yes | Filter by agent name (ownership, not participation) |
| `channel` | string | No | Filter by channel (comma-separated) |
| `limit` | integer | No | Max results (default 20, max 100) |

**Response:**
```json
{
  "sessions": [
    {
      "id": "uuid",
      "agent_id": "main",
      "user_id": "12345",
      "channel": "ui",
      "started_at": "2026-03-27T10:00:00Z",
      "last_message_at": "2026-03-27T10:05:00Z",
      "title": "Discussion about X",
      "metadata": {},
      "run_status": "idle",
      "participants": [],
      "parent_session_id": null,
      "end_reason": null
    }
  ],
  "total": 42
}
```

### DELETE /api/sessions

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `agent` | string | Conditional | Delete all sessions for this agent |
| `channel` | string | Conditional | Delete all sessions for this channel (comma-separated) |

One of `agent` or `channel` is required.

**Response:** `{ "ok": true, "deleted": 5 }`

### GET /api/sessions/search

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `q` | string | Required | Search query |
| `agent` | string | `"main"` | Filter by agent |
| `limit` | integer | 50 | Max results (max 200) |

**Response:**
```json
{
  "results": [
    {
      "content": "...",
      "session_id": "uuid",
      "user_id": "...",
      "channel": "ui",
      "role": "user",
      "created_at": "...",
      "rank": 0.95
    }
  ],
  "count": 3
}
```

### GET /api/sessions/stuck

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `stale_secs` | integer | 90 | Seconds without activity to consider stuck |
| `max_retries` | integer | 3 | Max retry count threshold |

**Response:** `{ "sessions": [{"id": "uuid", "agent_id": "main"}] }`

### GET /api/sessions/{id}

Returns lightweight session metadata for deep-link resolution. Does not require `agent` parameter.

**Response:** `{ "id": "uuid", "agent_id": "main", "channel": "ui", "run_status": "idle" }`

### PATCH /api/sessions/{id}

**Request body** (all fields optional):
```json
{
  "title": "New session title",
  "ui_state": { "key": "value" }
}
```

`ui_state` is merged into session metadata. Must be a JSON object under 1 KB.

### POST /api/sessions/{id}/compact

**Response:**
```json
{ "ok": true, "facts_extracted": 12, "new_message_count": 5 }
```

### GET /api/sessions/{id}/export

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `agent` | string | — | Ownership check |
| `format` | string | `json` | `json` or `markdown` |

Markdown export returns `Content-Disposition: attachment; filename="session-{id}.md"`.

### POST /api/sessions/{id}/invite

**Request body:**
```json
{ "agent_name": "Agent2" }
```

**Response:** `{ "participants": ["main", "Agent2"] }`

### POST /api/sessions/{id}/fork

Creates a branched user message. Enables conversation tree navigation.

**Request body:**
```json
{
  "branch_from_message_id": "uuid",
  "content": "New user message text"
}
```

**Response:**
```json
{
  "message_id": "uuid",
  "parent_message_id": "uuid",
  "branch_from_message_id": "uuid"
}
```

### POST /api/sessions/{id}/retry

Replays the last user message through the engine in a background task. Useful for recovering stuck sessions.

**Response:** `{ "ok": true, "retry_count": 1 }` or `409` if session not in running state.

### GET /api/sessions/{id}/messages

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `limit` | integer | 50 | Max results (max 200) |
| `agent` | string | **Required** | Owner check — request without it is rejected with `400` |
| `before_id` | uuid | — | Pagination cursor |

### POST /api/messages/{id}/feedback

**Request body:**
```json
{ "feedback": 1 }
```

Values: `1` = like, `-1` = dislike, `0` = clear.

### PATCH /api/messages/{id}

Edit a user message (role must be `user`). Requires `?agent=` query param.

**Request body:**
```json
{ "content": "Updated message text" }
```

---

## 7. Memory

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/memory` | List / search memory chunks |
| `POST` | `/api/memory` | Create a memory chunk manually |
| `GET` | `/api/memory/stats` | Memory statistics |
| `GET` | `/api/memory/export` | Export all memory as JSON |
| `GET` | `/api/memory/fts-language` | Get FTS language setting |
| `PUT` | `/api/memory/fts-language` | Set FTS language |
| `DELETE` | `/api/memory/{id}` | Delete a memory chunk |
| `PATCH` | `/api/memory/{id}` | Update a memory chunk |
| `GET` | `/api/memory/tasks` | List memory indexing tasks |
| `GET` | `/api/memory/documents` | List source documents |
| `GET` | `/api/memory/documents/{id}` | Get document details |
| `PATCH` | `/api/memory/documents/{id}` | Update document metadata |
| `DELETE` | `/api/memory/documents/{id}` | Delete document and its chunks |

### GET /api/memory

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `query` | string | — | Semantic/FTS search query |
| `limit` | integer | 20 | Max results (max 100) |
| `offset` | integer | 0 | Pagination offset |

When `query` is provided, performs hybrid semantic + FTS search. Without `query`, returns paginated list.

**Memory chunk object (search result):**
```json
{
  "id": "uuid",
  "content": "User prefers concise answers",
  "source": "shared",
  "relevance_score": 0.87,
  "similarity": 0.91,
  "pinned": false
}
```

### POST /api/memory

**Request body:**
```json
{
  "agent": "main",
  "content": "User's birthday is March 15",
  "pinned": true
}
```

### PUT /api/memory/fts-language

**Request body:**
```json
{ "language": "russian" }
```

Valid values: `simple`, `english`, `russian`, and other PostgreSQL text search configurations.

### PATCH /api/memory/{id}

**Request body** (all fields optional):
```json
{ "content": "Updated fact text", "pinned": true }
```

---

## 8. Tools and MCP

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/tool-definitions` | List all tool names visible to agents (system + YAML + MCP) |
| `GET` | `/api/tools` | List registered HTTP tool services |
| `POST` | `/api/tools` | Register a new tool service |
| `PUT` | `/api/tools/{name}` | Update a tool service |
| `DELETE` | `/api/tools/{name}` | Delete a tool service |
| `GET` | `/api/mcp` | List MCP servers |
| `POST` | `/api/mcp` | Register an MCP server |
| `PUT` | `/api/mcp/{name}` | Update an MCP server |
| `DELETE` | `/api/mcp/{name}` | Delete an MCP server |
| `POST` | `/api/mcp/{name}/reload` | Reload an MCP server |
| `POST` | `/api/mcp/{name}/toggle` | Enable or disable an MCP server |

### GET /api/tool-definitions

Returns a sorted list of all tool names available in the system (built-in + YAML + MCP).

**Response:** `{ "tools": ["memory_search", "workspace_write", "web_search", ...] }`

---

## 9. YAML Tools

YAML tools are HTTP-based tool definitions stored as `.yaml` files in `workspace/tools/`.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/yaml-tools` | List all YAML tools (all statuses) |
| `POST` | `/api/yaml-tools` | Create a new YAML tool |
| `GET` | `/api/yaml-tools/{tool}` | Get YAML tool definition |
| `PUT` | `/api/yaml-tools/{tool}` | Update a YAML tool |
| `DELETE` | `/api/yaml-tools/{tool}` | Delete a YAML tool |
| `POST` | `/api/yaml-tools/{tool}/verify` | Move tool to verified status |
| `POST` | `/api/yaml-tools/{tool}/disable` | Move tool to disabled status |
| `POST` | `/api/yaml-tools/{tool}/enable` | Re-enable a disabled tool |

Per-agent compatibility aliases:

| Method | Path |
|--------|------|
| `GET` | `/api/agents/{name}/yaml-tools` |
| `POST` | `/api/agents/{name}/yaml-tools/{tool}/verify` |
| `POST` | `/api/agents/{name}/yaml-tools/{tool}/disable` |

### POST /api/yaml-tools

**Request body:**
```json
{ "content": "name: get_weather\ndescription: ...\nmethod: GET\nendpoint: ...\n..." }
```

The `content` field is a YAML string. Tool is created with status `verified`.

**Tool statuses:**

| Status | Location | Description |
|--------|----------|-------------|
| `verified` | `workspace/tools/*.yaml` | Active, available to agents |
| `draft` | `workspace/tools/draft/*.yaml` | Work-in-progress, not yet active |
| `disabled` | `workspace/tools/disabled/*.yaml` | Archived, not available |

**YAML tool format:**
```yaml
name: get_weather
description: Get current weather for a location
method: GET
endpoint: https://api.example.com/weather
parameters:
  - name: location
    type: string
    description: City name
    required: true
auth:
  type: bearer_env
  key: WEATHER_API_KEY
response_transform: "$.current"
```

**Auth types:**

| type | Description |
|------|-------------|
| `bearer_env` | Read API key from env var named by `key` |
| `none` | No authentication |

---

## 10. Skills

Skills are Markdown files stored in `workspace/skills/`. Shared prompt fragments injected into agent context.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/skills` | List all skills |
| `GET` | `/api/skills/repairs` | List skill repair proposals |
| `PATCH` | `/api/skills/repairs/{id}` | Resolve a skill repair proposal |
| `GET` | `/api/skills/{skill}` | Get skill content |
| `PUT` | `/api/skills/{skill}` | Create or update a skill |
| `DELETE` | `/api/skills/{skill}` | Delete a skill |
| `GET` | `/api/skills/{skill}/versions` | List skill version history |
| `GET` | `/api/skills/{skill}/versions/{vid}` | Get a specific version |
| `POST` | `/api/skills/{skill}/versions/{vid}/restore` | Restore skill to a previous version |
| `POST` | `/api/skills/{skill}/snapshot` | Create a manual snapshot |
| `GET` | `/api/skills/{skill}/curator-decisions` | Get curator decisions for a skill |

Per-agent aliases:

| Method | Path |
|--------|------|
| `GET` | `/api/agents/{name}/skills` |
| `GET` | `/api/agents/{name}/skills/{skill}` |
| `PUT` | `/api/agents/{name}/skills/{skill}` |
| `DELETE` | `/api/agents/{name}/skills/{skill}` |

### PUT /api/skills/{skill}

**Request body:**
```json
{ "content": "# Web Search Strategy\n\nUse SearXNG for general queries..." }
```

---

## 11. Channels

Channels connect agents to messaging platforms (Telegram, Discord, etc.).

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/channels` | List all channels across all agents |
| `GET` | `/api/channels/active` | List currently connected channel adapters |
| `POST` | `/api/channels/notify` | Send a notification via a channel |
| `GET` | `/api/agents/{name}/channels` | List channels for an agent |
| `POST` | `/api/agents/{name}/channels` | Create a channel for an agent |
| `PUT` | `/api/agents/{name}/channels/{id}` | Update a channel |
| `DELETE` | `/api/agents/{name}/channels/{id}` | Delete a channel |
| `POST` | `/api/agents/{name}/channels/{id}/restart` | Restart a channel adapter |
| `POST` | `/api/agents/{name}/channels/{id}/ack` | Acknowledge a channel error |
| `GET` | `/api/agents/{name}/channels/{id}/status` | Get channel status |
| `GET` | `/api/agents/{name}/hooks` | Get agent hook configuration |
| `GET` | `/ws/channel/{agent_name}` | WebSocket endpoint for channel adapters |

### Channel object

```json
{
  "id": "uuid",
  "agent_name": "main",
  "channel_type": "telegram",
  "display_name": "My Bot",
  "config": {},
  "status": "running",
  "error_msg": null
}
```

### POST /api/agents/{name}/channels

**Supported channel types:** `telegram`, `discord`, `matrix`, `irc`, `slack`, `whatsapp`

**Request body:**
```json
{
  "channel_type": "telegram",
  "display_name": "My Bot",
  "config": { "bot_token": "5092435297:AAH..." }
}
```

Credential fields (`bot_token`, `access_token`, `password`, `app_token`, `verify_token`) are extracted from `config` and stored in the secrets vault. The returned `config` has these fields redacted.

**Response:** `{ "ok": true, "id": "uuid", "status": "stopped" }`

### POST /api/channels/notify

Send a notification through a channel without going through the agent LLM.

**Request body:**
```json
{
  "channel_id": "uuid",
  "text": "Notification message",
  "parse_mode": "MarkdownV2"
}
```

### /ws/channel/{agent_name}

Channel adapters connect via WebSocket. Authentication:
- `Authorization: Bearer <token>` header
- `?ticket=<uuid>` query parameter (from `POST /api/auth/ws-ticket`)

---

## 12. Cron Jobs

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/cron` | List all cron jobs |
| `POST` | `/api/cron` | Create a cron job |
| `PUT` | `/api/cron/{id}` | Update a cron job |
| `DELETE` | `/api/cron/{id}` | Delete a cron job |
| `POST` | `/api/cron/{id}/run` | Trigger a cron job immediately |
| `GET` | `/api/cron/{id}/runs` | Get run history for a job |
| `GET` | `/api/cron/runs` | Get run history for all jobs |

### POST /api/cron

**Request body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Unique job name |
| `agent` | string | Yes | Target agent name |
| `task` | string | Yes | Task message sent to the agent |
| `cron` | string | Conditional | Cron expression (required unless `run_once = true`) |
| `timezone` | string | No | IANA timezone (default: `UTC`) |
| `announce_to` | string/object | No | Channel to send output to |
| `silent` | bool | No | If true, discard agent output (default: false) |
| `jitter_secs` | integer | No | Random delay added to execution time |
| `run_once` | bool | No | One-shot job (requires `run_at`) |
| `run_at` | datetime | Conditional | ISO 8601 datetime for one-shot jobs |
| `tool_policy` | object | No | Tool policy override for this job |

**Job object:**
```json
{
  "id": "uuid",
  "name": "morning-briefing",
  "agent": "main",
  "cron": "0 9 * * *",
  "timezone": "UTC",
  "task": "Prepare daily briefing",
  "enabled": true,
  "silent": false,
  "announce_to": "telegram",
  "jitter_secs": 0,
  "run_once": false,
  "run_at": null,
  "created_at": "2026-01-01T00:00:00Z",
  "last_run": "2026-03-27T06:00:00Z",
  "next_run": "2026-03-28T06:00:00Z",
  "tool_policy": null
}
```

---

## 13. Approvals

Human-in-the-loop approval for sensitive tool calls. Approval endpoints are registered under `/api/agents` router.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/approvals` | List pending approvals |
| `POST` | `/api/approvals/{id}/resolve` | Approve or deny a pending action |
| `GET` | `/api/approvals/allowlist` | List auto-approved tools |
| `POST` | `/api/approvals/allowlist` | Add a tool to the allowlist |
| `DELETE` | `/api/approvals/allowlist/{id}` | Remove from allowlist |

### POST /api/approvals/{id}/resolve

**Request body:**
```json
{ "decision": "approve" }
```

Values: `"approve"` or `"deny"`.

### POST /api/approvals/allowlist

**Request body:**
```json
{ "tool_name": "workspace_write", "agent": "main" }
```

---

## 15. Webhooks

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/webhooks` | List all webhooks |
| `POST` | `/api/webhooks` | Create a webhook |
| `PUT` | `/api/webhooks/{id}` | Update a webhook |
| `DELETE` | `/api/webhooks/{id}` | Delete a webhook |
| `POST` | `/api/webhooks/{id}/regenerate-secret` | Regenerate webhook secret |
| `POST` | `/webhook/{name}` | Trigger endpoint (auth by webhook secret) |

### POST /api/webhooks

**Request body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Unique webhook name (used in trigger URL) |
| `agent` | string | Yes | Target agent name |
| `prompt_prefix` | string | No | Text prepended to payload before sending to agent |
| `enabled` | bool | No | Default: `true` |
| `webhook_type` | string | No | `generic` (default) or `github` |
| `event_filter` | array | No | GitHub webhooks: list of event types (e.g. `["push", "pull_request"]`) |

**Response:** `201 Created` with full webhook object including the **full secret** (only visible at creation).

### POST /api/webhooks/{id}/regenerate-secret

**Response:** `{ "ok": true, "secret": "new-64-char-hex-string" }`

### POST /webhook/{name}

Trigger endpoint called by external systems. **Not** behind standard auth middleware — authenticated by webhook secret.

**Auth methods:**

| Type | Auth method |
|------|-------------|
| `generic` | `Authorization: Bearer <secret>` |
| `github` | `X-Hub-Signature-256: sha256=<hmac>` |

**Query parameters:**

| Parameter | Description |
|-----------|-------------|
| `async=true` | Return immediately; process payload in background |

**Rate limiting:** 5 auth failures within 5 minutes locks the webhook for 10 minutes.

**Response (sync):** `{ "ok": true, "response": "Agent response text" }`  
**Response (async):** `{ "ok": true, "queued": true }`

---

## 16. Secrets

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/secrets` | List all secrets (values masked) |
| `POST` | `/api/secrets` | Create or update a secret |
| `GET` | `/api/secrets/{name}` | Get a secret |
| `DELETE` | `/api/secrets/{name}` | Delete a secret |

### POST /api/secrets

**Request body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Secret name |
| `value` | string | Conditional | Secret value (required unless updating description only) |
| `description` | string | No | Human-readable description |
| `scope` | string | No | Agent name for per-agent secrets; empty for global |

**Resolution order:** `(name, scope)` → `(name, "")` global → environment variable.

### GET /api/secrets/{name}

| Parameter | Type | Description |
|-----------|------|-------------|
| `scope` | string | Agent scope (empty for global) |
| `reveal` | bool | Return plaintext value (default: false) |

---

## 17. Config

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/config` | Get gateway configuration |
| `PUT` | `/api/config` | Update gateway configuration |
| `GET` | `/api/config/export` | Export full config as JSON |
| `POST` | `/api/config/import` | Import config from JSON |
| `GET` | `/api/config/schema` | Get JSON schema for gateway config |
| `POST` | `/api/restart` | Restart the gateway process |

### GET /api/config/schema

Returns JSON Schema for `config/hydeclaw.toml`. Useful for UI config editors and client-side validation.

### POST /api/restart

Signals the process to exit (systemd or watchdog will restart it).

**Response:** `{ "ok": true, "message": "restarting..." }`

---

## 18. Backup and Restore

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/backup` | List available backups |
| `POST` | `/api/backup` | Create a new backup |
| `GET` | `/api/backup/{filename}` | Download a backup file |
| `DELETE` | `/api/backup/{filename}` | Delete a backup file |
| `POST` | `/api/restore` | Restore from a backup |

### POST /api/backup

Creates a full backup to the `backups/` directory.

**Response:**
```json
{
  "ok": true,
  "filename": "hydeclaw-backup-2026-03-27T10-00-00Z.json",
  "path": "backups/hydeclaw-backup-2026-03-27T10-00-00Z.json"
}
```

### POST /api/restore

Body size limit: configured by `limits.max_restore_size_mb` (default 500 MB). Uses chunked streaming validation — axum's default 2 MB limit is disabled for this endpoint.

**Request body:**
```json
{ "filename": "hydeclaw-backup-2026-03-27T10-00-00Z.json" }
```

---

## 19. Services

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/services` | List all managed services (Docker + native processes) |
| `POST` | `/api/services/{name}/{action}` | Perform an action on a service |
| `POST` | `/api/containers/{name}/restart` | Restart a Docker container (whitelist-restricted) |

### POST /api/services/{name}/{action}

**Actions for Docker services:** `restart`, `rebuild`, `start`, `stop`, `status`, `logs`

**Actions for native managed processes** (channels, toolgate): `restart`, `start`, `stop`, `status`, `logs`

`rebuild` is Docker-only. For native processes, `restart` and `rebuild` both call `pm.restart()`.

**Response:**
```json
{ "ok": true, "action": "restart", "service": "toolgate", "managed": true }
```

### POST /api/containers/{name}/restart

Whitelist-only — only non-sensitive containers (browser-renderer, searxng, mcp-*). PostgreSQL container is excluded.

---

## 20. Watchdog

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/watchdog/status` | Current watchdog status (reads `/tmp/hydeclaw-watchdog.json`) |
| `GET` | `/api/watchdog/config` | Read watchdog TOML config |
| `PUT` | `/api/watchdog/config` | Update watchdog TOML config |
| `GET` | `/api/watchdog/settings` | Read DB-backed alerting settings |
| `PUT` | `/api/watchdog/settings` | Update alerting settings |
| `POST` | `/api/watchdog/restart/{name}` | Execute restart command for a check |

These endpoints are registered in `monitoring.rs`, not a separate watchdog handler.

### PUT /api/watchdog/config

**Request body:**
```json
{ "config": "# TOML content\n[global]\n..." }
```

Config is validated as valid TOML before saving.

### PUT /api/watchdog/settings

| Key | Type | Description |
|-----|------|-------------|
| `alert_channel_ids` | array | Channel UUIDs to send alerts to |
| `alert_events` | array | Event types that trigger alerts |

---

## 21. Providers

All providers (LLM and media) share the `/api/providers` endpoint. Distinguished by `kind` field (`"text"`, `"stt"`, `"tts"`, `"vision"`, `"imagegen"`, `"embedding"`).

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/provider-types` | List supported LLM provider types |
| `GET` | `/api/media-drivers` | List available media driver types |
| `GET` | `/api/media-config` | Export toolgate-compatible media config |
| `GET` | `/api/providers` | List configured providers (filter by `?kind=`) |
| `POST` | `/api/providers` | Create a provider |
| `GET` | `/api/providers/{id}` | Get a provider |
| `PUT` | `/api/providers/{id}` | Update a provider |
| `PATCH` | `/api/providers/{id}` | Patch CLI options for a provider |
| `DELETE` | `/api/providers/{id}` | Delete a provider |
| `GET` | `/api/providers/{id}/models` | List models from this provider |
| `GET` | `/api/providers/{id}/resolve` | Resolve connection details |
| `POST` | `/api/providers/{id}/test-cli` | Test CLI-based provider |
| `GET` | `/api/provider-active` | Get currently active provider per capability |
| `PUT` | `/api/provider-active` | Set active provider for a capability |

### POST /api/providers (LLM)

**Request body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Human-readable connection name (alphanumeric, `-`, `_`) |
| `kind` | string | Yes | `"text"` for LLM providers |
| `provider_type` | string | Yes | Provider type ID (e.g. `openai`, `anthropic`, `minimax`) |
| `base_url` | string | No | Override base URL |
| `api_key` | string | No | API key (stored in vault, masked in responses) |
| `default_model` | string | No | Default model for this connection |
| `notes` | string | No | Internal notes |

### Valid `kind` values

| Kind | Description |
|------|-------------|
| `text` | LLM text generation provider |
| `stt` | Speech-to-text |
| `tts` | Text-to-speech |
| `vision` | Image description / visual understanding |
| `imagegen` | Image generation |
| `embedding` | Text embeddings |

### PUT /api/provider-active

**Request body:**
```json
{
  "stt": "whisper-local",
  "tts": "qwen3-tts-voice",
  "vision": "qwen35-local",
  "embedding": "local-embed"
}
```

Omit a capability to leave its active provider unchanged.

---

## 22. TTS and Canvas

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/tts/voices` | List available TTS voices |
| `POST` | `/api/tts/synthesize` | Synthesize speech |
| `GET` | `/api/canvas/{agent}` | Get current canvas state |
| `DELETE` | `/api/canvas/{agent}` | Clear canvas state |

### POST /api/tts/synthesize

**Request body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `text` | string | Yes | Text to synthesize |
| `voice` | string | No | Voice name or clone identifier (e.g. `clone:MyVoice`) |

**Response:** Audio binary with appropriate `Content-Type` header (`audio/mpeg`, `audio/ogg`, etc.), or JSON error.

### GET /api/canvas/{agent}

**Response:**
```json
{
  "visible": true,
  "agent": "main",
  "action": "present",
  "content_type": "markdown",
  "content": "# Current canvas content\n...",
  "title": null
}
```

When canvas is empty: `{ "visible": false }`.

---

## 23. Media Upload

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/api/media/upload` | Required | Upload a file (max 20 MB) |
| `POST` | `/api/media/transcribe` | Required | Transcribe audio via STT (max 20 MB) |
| `POST` | `/api/vision/analyze` | Required | Analyze an image via vision provider |
| `GET` | `/uploads/{filename}` | Public / Signed | Serve an uploaded file |

### POST /api/media/upload

Multipart form upload. Saves to `workspace/uploads/{uuid}.{ext}`.

**Allowed extensions:** `jpg`, `jpeg`, `png`, `gif`, `webp`, `bmp`, `ico`, `mp4`, `webm`, `mov`, `avi`, `ogg`, `oga`, `mp3`, `wav`, `flac`, `aac`, `m4a`, `pdf`, `docx`, `xlsx`, `pptx`, `txt`, `md`, `csv`, `log`, `json`, `toml`, `yaml`, `yml`, `zip`, `tar`, `gz`, `bin`. Other extensions saved as `.bin`.

**Response:**
```json
{ "url": "http://host:18789/uploads/uuid.jpg", "filename": "uuid.jpg", "size": 204800 }
```

### POST /api/media/transcribe

Multipart audio upload. Proxies to Toolgate `/transcribe`.

| Query param | Default | Description |
|-------------|---------|-------------|
| `lang` | `"ru"` | Language hint for transcription |

**Supported audio extensions:** `webm`, `mp4`, `ogg`, `oga`, `mp3`, `wav`, `m4a`, `aac`, `flac`.

**Response:** `{ "text": "<transcript>" }` or `503` if STT not configured.

### GET /uploads/{filename}

Serves uploaded files. Security: HMAC-signed URL enforcement is configurable via `uploads.require_signature`.

- `require_signature = true`: `403` when `?sig=&exp=` missing.
- `require_signature = false` (default): unsigned OK; if signature present it is still validated.
- Expired signature: `410 Gone`.
- Invalid signature: `403 Forbidden`.
- Path traversal (`..`, `/`, `\`): `400 Bad Request`.

---

## 24. Workspace

Browse, read, write, and delete files within the `workspace/` directory.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/workspace` | Browse workspace root |
| `GET` | `/api/workspace/{*path}` | List directory or read file |
| `PUT` | `/api/workspace/{*path}` | Write a file |
| `DELETE` | `/api/workspace/{*path}` | Delete a file |

All paths are strictly sandboxed within `workspace/`. Symlink-following path traversal is rejected with `403 Forbidden`.

### GET /api/workspace/{*path}

For **directories**: returns JSON listing.
```json
{
  "entries": [
    { "name": "tools", "is_dir": true, "display": "tools/ (4.2 KB)" },
    { "name": "notes.md", "is_dir": false, "display": "notes.md (1.2 KB)" }
  ]
}
```

For **files**: returns raw file content with appropriate `Content-Type`.

### PUT /api/workspace/{*path}

**Request body:** Raw file content (any content type). Parent directories created automatically.

---

## 25. Workspace Files (Signed URLs)

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/workspace-files/{*path}` | HMAC signature | Serve workspace artifacts via signed URL |

HMAC-signed access to workspace files created by tools (`workspace_write`, `workspace_edit`, `code_exec`). No Bearer token required — security is via `?sig=<hmac>&exp=<unix_ts>`.

Signature payload: `HMAC-SHA256("{path}:{exp}", upload_key)`.

Returns `403 Forbidden` for invalid/expired signatures, `403` for path escaping workspace.

---

## 26. OAuth

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/api/oauth/callback` | Public | OAuth callback (called by OAuth provider) |
| `GET` | `/api/oauth/providers` | Required | List supported OAuth providers |
| `GET` | `/api/oauth/accounts` | Required | List configured OAuth accounts |
| `POST` | `/api/oauth/accounts` | Required | Create an OAuth account |
| `DELETE` | `/api/oauth/accounts/{id}` | Required | Delete an OAuth account |
| `POST` | `/api/oauth/accounts/{id}/connect` | Required | Initiate OAuth authorization flow |
| `POST` | `/api/oauth/accounts/{id}/revoke` | Required | Revoke OAuth tokens |
| `GET` | `/api/agents/{name}/oauth/bindings` | Required | List agent OAuth bindings |
| `POST` | `/api/agents/{name}/oauth/bindings` | Required | Bind OAuth account to agent |
| `DELETE` | `/api/agents/{name}/oauth/bindings/{provider}` | Required | Remove OAuth binding |

### POST /api/oauth/accounts

**Request body:**
```json
{
  "provider": "google",
  "display_name": "Work Google Account",
  "client_id": "xxx.apps.googleusercontent.com",
  "client_secret": "GOCSPX-..."
}
```

### POST /api/oauth/accounts/{id}/connect

Generates authorization URL. Redirect user to this URL to complete OAuth.

**Response:** `{ "auth_url": "https://accounts.google.com/o/oauth2/auth?..." }`

### POST /api/agents/{name}/oauth/bindings

**Request body:** `{ "account_id": "uuid" }`

---

## 27. Access / Pairing

Agents with `access.mode: allowlist` require users to be approved before chatting. The pairing flow uses a 6-character code.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/access/{agent}/pending` | List pending pairing requests |
| `POST` | `/api/access/{agent}/approve/{code}` | Approve a pairing request |
| `POST` | `/api/access/{agent}/reject/{code}` | Reject a pairing request |
| `GET` | `/api/access/{agent}/users` | List approved users |
| `DELETE` | `/api/access/{agent}/users/{user_id}` | Remove user from allowlist |

### Pairing flow

1. User sends `/start` or pairing code to the bot.
2. Core creates a pending entry with a 6-character code.
3. Admin calls `POST /api/access/{agent}/approve/{code}`.
4. User is added to `allowed_users`.

### GET /api/access/{agent}/pending

**Response:**
```json
{
  "pending": [
    {
      "code": "ABC123",
      "user_id": "123456789",
      "channel": "telegram",
      "created_at": "2026-03-27T10:00:00Z"
    }
  ]
}
```

### GET /api/access/{agent}/users

**Response:**
```json
{
  "users": [
    {
      "channel_user_id": "123456789",
      "display_name": "User",
      "approved_at": "2026-01-15T12:00:00Z"
    }
  ]
}
```

---

## 28. WebSocket (UI Events)

| Path | Auth | Description |
|------|------|-------------|
| `GET /ws` | Ticket or Bearer | UI real-time event stream |

Authentication:
- `?ticket=<uuid>` query parameter (from `POST /api/auth/ws-ticket`)
- `Authorization: Bearer <token>` header on upgrade request

### UI event types

| Event | Description |
|-------|-------------|
| `agent_processing` | Agent started/stopped processing |
| `session_updated` | Session metadata or messages changed |
| `cron_completed` | A scheduled cron job finished |
| `task_updated` | Task status changed |
| `approval_pending` | New tool approval request |
| `approval_resolved` | Approval was resolved |
| `channel_status` | Channel adapter connected/disconnected |
| `memory_updated` | Memory chunk created or updated |
| `agent_joined` | Agent invited into a multi-agent session |
| `log` | Real-time log line |

---

## 29. Email Triggers (Gmail)

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/api/triggers/email` | Required | List Gmail triggers |
| `POST` | `/api/triggers/email` | Required | Create a Gmail trigger |
| `DELETE` | `/api/triggers/email/{id}` | Required | Delete a Gmail trigger |
| `POST` | `/api/triggers/email/push` | Public | Gmail Pub/Sub push endpoint |

### POST /api/triggers/email

**Request body:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `agent` | string | Yes | Target agent name |
| `oauth_account_id` | string | Yes | UUID of connected Google OAuth account |
| `label_filter` | array | No | Gmail label IDs to filter (e.g. `["INBOX"]`) |
| `prompt_prefix` | string | No | Text prepended to email content |

Gmail watch subscription is automatically registered with Google Pub/Sub.

### POST /api/triggers/email/push

Called by Google Pub/Sub when new mail arrives. No Bearer token required.

---

## 30. GitHub Integration

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/agents/{name}/github/repos` | List allowed GitHub repos for an agent |
| `POST` | `/api/agents/{name}/github/repos` | Add a GitHub repo to the allowlist |
| `DELETE` | `/api/agents/{name}/github/repos/{id}` | Remove a repo from the allowlist |

### POST /api/agents/{name}/github/repos

**Request body:**
```json
{ "owner": "octocat", "repo": "hello-world" }
```

---

## 31. Setup

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/api/setup/status` | Public | Whether initial setup has been completed |
| `GET` | `/api/setup/requirements` | Public | Prerequisites checklist (Docker, DB, disk space) |
| `POST` | `/api/setup/complete` | Required | Mark setup as complete (guarded — `403` after completion) |

### GET /api/setup/status

**Response:**
```json
{ "needs_setup": true }
```

`needs_setup` is derived from the `system_flags` table (not agent count).

### GET /api/setup/requirements

Returns a checklist for the setup wizard. Public endpoint — no token needed.

**Response:**
```json
{
  "requirements": [
    { "name": "database", "ok": true, "message": "PostgreSQL 17 reachable" },
    { "name": "master_key", "ok": true, "message": "HYDECLAW_MASTER_KEY set" },
    { "name": "provider", "ok": false, "message": "No LLM provider configured" },
    { "name": "agent", "ok": false, "message": "No agents created" }
  ]
}
```

### POST /api/setup/complete

Marks the instance as fully configured. Protected by `setup_guard_middleware` — returns `403` if already completed.

**Request body:**
```json
{ "provider": "openai", "model": "gpt-4o-mini", "agent_name": "assistant" }
```

**Response:** `{ "ok": true }`

---

## 32. Network

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/network/addresses` | List detected network addresses (LAN, WAN, Tailscale) |

### GET /api/network/addresses

Returns WAN IP (with CGNAT detection), Tailscale status, LAN interfaces, and mDNS hostname. WAN IP cached for 5 minutes.

**Response:**
```json
{
  "wan_ip": "1.2.3.4",
  "wan_cgnat": false,
  "tailscale": { "status": "...", "ip": "100.x.x.x" },
  "interfaces": [
    { "name": "eth0", "ip": "192.168.1.85", "family": "ipv4" }
  ],
  "mdns_hostname": "hydeclaw.local",
  "port": 18789
}
```

---

## 33. Notifications

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/notifications` | List notifications |
| `PATCH` | `/api/notifications/{id}` | Mark a notification as read |
| `POST` | `/api/notifications/read-all` | Mark all notifications as read |
| `DELETE` | `/api/notifications/clear` | Delete all read notifications |

### GET /api/notifications

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `limit` | integer | 50 | Max results (max 200) |
| `offset` | integer | 0 | Pagination offset |

**Response:**
```json
{
  "items": [
    {
      "id": "uuid",
      "type": "agent_error",
      "title": "Agent failed",
      "body": "Provider returned 401",
      "read": false,
      "created_at": "2026-04-06T12:00:00Z",
      "data": {}
    }
  ],
  "unread_count": 3,
  "limit": 50,
  "offset": 0
}
```

Note: Backend serializes `notification_type` as `"type"` in JSON.

### PATCH /api/notifications/{id}

**Request body:** `{ "read": true }`  
**Response:** `{ "ok": true }`

### POST /api/notifications/read-all

**Response:** `{ "ok": true, "updated": 5 }`

### DELETE /api/notifications/clear

Deletes read notifications only. Unread preserved.

**Response:** `{ "ok": true, "deleted": 12 }`

---

## 34. Curator

The curator is an automated skill maintenance system that reviews, repairs, and archives skills.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/curator/status` | Current curator status and last run info |
| `GET` | `/api/curator/config` | Get curator configuration |
| `PUT` | `/api/curator/config` | Update curator configuration |
| `POST` | `/api/curator/run` | Trigger a curator run manually |
| `GET` | `/api/curator/runs` | List curator run history |
| `GET` | `/api/curator/runs/{id}` | Get details of a curator run |
| `GET` | `/api/curator-decisions/recent` | Most recent curator decision per skill |
| `GET` | `/api/skills/{skill}/curator-decisions` | Curator decisions for a specific skill |

### GET /api/curator/status

**Response:**
```json
{
  "enabled": true,
  "cron": "0 3 * * *",
  "last_run_at": "2026-05-01T03:00:00Z",
  "last_run_id": "uuid",
  "last_phase1": 12,
  "last_phase2": 3,
  "last_phase3": 1
}
```

### GET /api/curator/config

**Response:**
```json
{
  "enabled": true,
  "cron": "0 3 * * *",
  "min_idle_minutes": 30,
  "stale_after_days": 90,
  "archive_after_days": 180,
  "max_repairs_per_run": 5,
  "agent_name": "main"
}
```

### GET /api/curator-decisions/recent

Returns most recent decision per skill as a flat map.

**Response:**
```json
{
  "web-search": {
    "action": "keep",
    "reason": "...",
    "decided_at": "2026-05-01T03:00:00Z"
  }
}
```

---

## 35. Session Failures

Read-only API for the structured session failure log.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/sessions/failures` | Paginated failure list |
| `GET` | `/api/sessions/{session_id}/failures` | Failure records for one session |

### GET /api/sessions/failures

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `agent` | string | — | Filter by agent name |
| `limit` | integer | 50 | Max results |
| `offset` | integer | 0 | Pagination offset |

**Response:**
```json
{
  "failures": [
    {
      "id": "uuid",
      "session_id": "uuid",
      "agent_id": "main",
      "failed_at": "2026-04-01T12:00:00Z",
      "failure_kind": "provider_error",
      "error_message": "Provider returned 500",
      "retry_count": 2,
      "resolved": false
    }
  ],
  "total": 5
}
```

---

## 36. CSP Reports

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/api/csp-report` | Public | Receive Content Security Policy violation reports |

Browser CSP report endpoint. Rate-limited separately from standard API endpoints. Body capped at 64 KB. Reports are aggregated into metrics counters visible in `/api/health/dashboard`.

---

## Error Responses

All error responses use a consistent JSON format:

```json
{ "error": "human-readable error message" }
```

**Common HTTP status codes:**

| Status | Meaning |
|--------|---------|
| `400` | Bad request — missing or invalid parameters |
| `401` | Unauthorized — missing or invalid Bearer token |
| `403` | Forbidden — path traversal, ownership mismatch, or feature guard |
| `404` | Not found |
| `409` | Conflict — resource already exists or concurrent state issue |
| `410` | Gone — signed URL expired |
| `413` | Payload too large — file exceeds 20 MB |
| `429` | Too many requests — rate limit exceeded or lockout active |
| `500` | Internal server error |
| `503` | Service unavailable — dependency not configured (embeddings, STT, etc.) |

---

## Notes

### Auth Lockout

500 failed auth attempts (per IP) triggers a 30-second lockout for requests without a valid `Authorization` header. Loopback addresses are exempt. Authenticated requests are exempt from rate limiting.

### LLM Retry Policy

Failed LLM calls are retried up to 3 times with exponential backoff. Retries triggered on HTTP `429`, `500`, `502`, `503`.

### Secrets Resolution Order

For any secret name and agent scope: `(name, scope)` → `(name, "")` global → environment variable.

### SSE Backpressure

Chat SSE endpoint uses bounded channels (256/512 events) with backpressure. Overflow events are dropped and counted in `/api/health/dashboard` under `sse_events_dropped_total`.

### CORS

CORS origins configured via `gateway.cors_origins`. If empty, allows the UI port (`:5173`) and API port on the same host.

### Session Branching

`parent_message_id` and `branch_from_message_id` on messages enable conversation tree navigation. `POST /api/sessions/{id}/fork` creates a new branch; `GET /api/sessions/{id}/active-path` returns the current active path through the tree.
