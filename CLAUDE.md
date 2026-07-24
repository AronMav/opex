# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
# Rust
make check              # cargo check --all-targets
make test               # cargo test (skips DB-backed tests if DATABASE_URL unset)
make test-db            # boots isolated postgres on :5434 + runs full suite
make lint               # cargo clippy --all-targets -- -D warnings
make audit              # cargo audit (RustSec advisories — see .cargo/audit.toml)
cargo test test_name -- --nocapture  # single test

# 263 tests under #[sqlx::test] need a live Postgres + DATABASE_URL.
# `cargo test` without DB will fail them with "EnvVar(NotPresent)" — that's
# expected; use `make test-db` (or set DATABASE_URL) to run the full suite.

# UI (Next.js)
cd ui && npm run build  # production build
cd ui && npm test       # vitest (one-shot)
cd ui && npm run dev    # dev server (port 3000)

# Channel adapter (Bun)
cd channels && bun test
```

## Deploy

**Canonical workflow (since 2026-06-18): build on the production server.**
OPEX runs on `aronmav@188.246.224.118` (x86_64, i7-8700 / 12T / 31GB). Source tree is cloned at `~/opex-src` on the server. The Pi is out of the ecosystem (no longer deployed to); all Pi-specific make targets / `PI_HOST` config were removed. Configure the server in `.deploy.env` (`SERVER_HOST`, `SERVER_DIR`).

```bash
make remote-deploy      # git pull on server → cargo build --release → atomic swap + restart
make remote-build       # build only on server (no swap, no restart) — for CI-style checks
make doctor             # GET /api/doctor health check
make logs               # journalctl --user -u opex-core -f
```

`remote-deploy` shells out to `~/opex-src/scripts/server-deploy.sh`. The script does:

1. `git pull --ff-only` in `~/opex-src`
2. `cargo build --release -p opex-core -p opex-watchdog -p opex-memory-worker`
3. Atomic mv each binary into `~/opex/{crate}-x86_64` (`.new` + `mv -f` — overwrite-safe for mmap'd binaries)
4. `systemctl --user restart` for each of the 3 services

Build time on the server: ~2m 50s cold, ~10–60s incremental.

### Legacy: local cross-compile + scp

Use only when push to remote is undesired or the server is busy.

```bash
make build-x86_64              # cargo zigbuild --target x86_64-unknown-linux-gnu (server fallback)
make deploy-binary-server      # build x86_64 + scp + restart on SERVER_HOST (manual server deploy)
```

**Why zigbuild for legacy path:** no OpenSSL anywhere — `reqwest` uses `rustls-tls` only. All crates in `Cargo.toml` use `rustls-tls` feature flags. Never add OpenSSL dependencies.

### LSP servers (host)

The `lsp` tool provides agents IDE-grade intelligence (diagnostics, definition, references, hover, symbols, rename) on Python projects in the workspace. v1 supports Python (pyright) only; TypeScript and Rust are planned for v2.

Language servers run as **host subprocesses** (not sandboxed), so the deploy server must have **Node.js + pyright** installed:

```bash
apt install -y nodejs npm
npm i -g pyright
pyright-langserver --version  # verify install
```

The tool is **gated by `[lsp] enabled = true` in `config/opex.toml`** (default off). If pyright is not on PATH, the tool returns a clear error and is otherwise inert.

## Release

```bash
./release.sh          # build for host architecture
./release.sh --all    # build for aarch64 + x86_64
```

**Version:** single source of truth is `Cargo.toml` (`[workspace.package] version`). Pass version to `release.sh` as argument — it syncs to `Cargo.toml`, `ui/package.json`, `channels/package.json` before building. Releases are published via git tag: `git tag v0.2.0 && git push origin v0.2.0`.

**Scripts in release archive:**

- `setup.sh` — interactive installer (fresh install)
- `update.sh` — one-command updater (`~/opex/update.sh opex-v0.2.0.tar.gz`)
- `uninstall.sh` — complete removal

**Install paths (server, x86_64):**

- Binary: `~/opex/opex-core-x86_64`
- Watchdog: `~/opex/opex-watchdog-x86_64`
- Memory worker: `~/opex/opex-memory-worker-x86_64`
- UI static: `~/opex/ui/out/`
- Config: `~/opex/config/`
- Workspace: `~/opex/workspace/`
- Migrations: `~/opex/migrations/`
- Docker: `~/opex/docker/`

## Architecture

OPEX is a Rust-based AI gateway. The core binary (`crates/opex-core`) handles everything: HTTP API, agent lifecycle, LLM calls, tool execution, channel bridging, memory, and secrets.

### Agent Engine (`src/agent/`)

Four entry points on `AgentEngine`, all thin adapters in [engine/run.rs](crates/opex-core/src/agent/engine/run.rs) that construct an `EventSink` and delegate to `pipeline::execute`:

- `handle_sse` — web SSE via `SseSink` (over `EngineEventSender`/flume)
- `handle_with_status` — channel adapters (Telegram/Discord) with typing indicator via `ChannelStatusSink` (two `UnboundedSender` channels)
- `handle_streaming` — plain-chunk text via `ChunkSink`
- `handle_isolated_via_pipeline` — RPC-style cron/agent-to-agent calls via `NoopSink`; returns final assistant text. Constructs `BehaviourLayers::for_cron(...)` so fallback provider, auto-continue, session-corruption recovery, tool-policy override, and forced-final LLM call all engage with cron-defaults.

Unified pipeline lives in [src/agent/pipeline/](crates/opex-core/src/agent/pipeline/):

- `sink.rs` — `EventSink` trait, `PipelineEvent` (`Stream(StreamEvent)` | `Phase(ProcessingPhase)`), `SinkError`, four production sinks (`SseSink`, `ChannelStatusSink`, `ChunkSink`, `NoopSink`)
- `bootstrap.rs` — session entry, user-message persist, timeline `running`, `ProcessingGuard`, slash-command detection. Same code path drives both SSE (`use_history=true`) and cron (`force_new_session=true, use_history=false`)
- `execute.rs` — main LLM+tools loop, transport-agnostic. Takes `&BehaviourLayers` parameter (see below)
- `finalize.rs` — single exit point: persist assistant or partial, timeline `done|failed|interrupted` via `SessionLifecycleGuard`, enqueue knowledge extraction
- `behaviour.rs` — five composable opt-in policy structs (`FallbackPolicy`, `AutoContinuePolicy`, `SessionRecoveryPolicy`, `ToolPolicyOverride`, `ForcedFinalCallPolicy`) bundled into `BehaviourLayers`. SSE callers use `BehaviourLayers::none()`; cron callers use `BehaviourLayers::for_cron(loop_config, msg)`. Each layer adds zero hot-path branches when disengaged. See [docs/architecture/2026-05-06-llm-loop-unification-plan.md](docs/architecture/2026-05-06-llm-loop-unification-plan.md).

**Key execution paths:**
- `pipeline::execute::execute()` — LLM call + tool loop, transport-agnostic
- `pipeline::handlers::*` — tool implementations (workspace_write, workspace_read, etc.)
- `workspace.rs::is_read_only()` — path protection

**Loop detection (`tool_loop.rs`):** Two-phase `LoopDetector` — `check_limits()` (pre-execution, read-only) + `record_execution()` (post-execution, tracks success/failure). Error-aware: 3 consecutive errors on same tool → break. Session timeline records lifecycle events for diagnostics and LoopDetector warm-up. LoopDetector resets on each new session entry; warm-up from timeline only runs for `ResumeRunning` and `ExplicitResume` re-entry modes. See design spec at [docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md](docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md).

**Session-scoped agents (`session_agent_pool.rs` + `engine_agent_tool.rs`):** Unified `agent` tool (ask/status/kill) replaces old `subagent` + `handoff` tools. Agents are always-alive peers bound to a session via `SessionAgentPool` in `AppState.session_pools`. Each `LiveAgent` holds its own LLM dialog context in memory, receives messages via mpsc channel, and processes them in a background tokio task using `run_subagent()`. Polling-based — no automatic routing or turn loop. Peer-to-peer: any agent in a session can spawn, message, or kill any other.

**Agent config** (TOML at `config/agents/{name}.toml`):
- `base = true` — system agent: can't be renamed/deleted, runs on host (no sandbox), can write to service dirs and tools
- `base = true` — cannot be renamed/deleted via API; SOUL.md + IDENTITY.md are immutable
- `SELF.md` (soul-enabled agents only) — written ONLY by the in-core reflection engine, never by agents: write-protected from `workspace_write`/`workspace_edit` (`is_read_only()` blocks it for base AND non-base agents alike) and rename-guarded in both directions (`workspace_rename` refuses renaming it away or overwriting it via rename). It is NOT injected via `WORKSPACE_FILES` — the prompt gets a re-serialized rendering of it inside a dedicated framing block (`agent/soul/self_md.rs::render_self_block`, wired in `context_builder.rs`).
- Both flags are **never** changed via PUT API — preserved from disk on every update
- Agent rename updates ~20 DB tables in a transaction (`TABLES_WITH_AGENT_ID_NOT_NULL` + `TABLES_WITH_AGENT_ID_NULLABLE` for the nullable `messages.agent_id`; plus the `agent_name`-keyed tables in `TABLES_WITH_AGENT_NAME`). Agent **deletion** uses a separate three-class classification (Ephemeral / History / DropRipe) in `gateway/handlers/agents/crud.rs` — Ephemeral tables are deleted, History (`TABLES_HISTORY_AGENT_ID`) survives unless `?purge_history=true`, `memory_chunks` private-scope is deleted + shared-scope anonymized, and soul biography is backed up fail-closed before any destructive step. A drift test + `agent_table_classification` doctor check guard against unclassified new tables. See [docs/runbooks/agent-deletion.md](docs/runbooks/agent-deletion.md).
- `[agent.soul]` (optional, `enabled = false` by default) — opts an agent into the autobiographical memory + reflection layer (events, periodic reflection, SELF.md). See [docs/superpowers/specs/2026-07-09-agent-soul-foundation-design.md](docs/superpowers/specs/2026-07-09-agent-soul-foundation-design.md).

**Subagent delegation:** `[agent.delegation]` section (optional) controls how the
`agent` tool spawns subagents:

- `max_depth = 1` (default) — subagents CANNOT recursively spawn further
  subagents
- `blocked_tools_extra = [...]` — extends the built-in deny-list
  (`SUBAGENT_DENIED_TOOLS`) at runtime AND in the visibility list
  The built-in `SUBAGENT_DENIED_TOOLS` cannot be weakened by subagents —
  `runtime_subagent_denylist` hard-anchors that constant (used by both the
  subagent runner and the visibility list). See `docs/ARCHITECTURE.md`
  §DelegationConfig for the rationale.

### Gateway (`src/gateway/`)

Axum HTTP API on port 18789. **Sub-router pattern:** 46 handler modules each export `pub(crate) fn routes() -> Router<AppState>`; `mod.rs` composes them via `.merge()`. Key handlers:

- `agents.rs` — CRUD for agent configs; sorts base agents first
- `chat/` — SSE streaming chat split by route family. `chat/mod.rs` (`routes()` only) merges:
  - `openai_compat.rs` — `/v1/chat/completions`
  - `models.rs` — `/v1/models`
  - `embeddings.rs` — `/v1/embeddings`
  - `sse.rs` — `/api/chat` (request parse + spawn)
  - `sse_converter.rs` — `StreamEvent` → SSE-JSON converter loop (AUDIT:SSE-01/02/03 invariants)
  - `resume.rs` — `/api/chat/{id}/stream` resume + replay
  - `streaming_db.rs` — `StreamingMessageGuard` + `upsert_streaming_append` etc.
  - `misc.rs` — `/health`, `/api/chat/{id}/abort`, model-override
- `sessions.rs` — session CRUD + fork endpoint (`POST /api/sessions/{id}/fork`) + active-path endpoint
- `services.rs` — managed native processes (channels, toolgate) + Docker container management (MCP, browser-renderer)
- `state.rs: agent_names()` / `agent_summaries()` — return agents sorted base-first then alphabetical

**Rate limiting:** 300 rpm default (configurable via `limits.max_requests_per_minute`). Authenticated requests (valid Bearer token) exempt. Auth lockout: 500 failed attempts → 30s block for requests without Authorization header. Loopback exempt.

### Tools (`src/tools/`)

**System tools:** registered in `agent/tool_registry.rs` (memory, workspace_write, workspace_edit, code_exec, agent, etc.). Tool policy `deny` list applies to ALL tools including core system tools — deny is checked first, before the core tools allowlist.

**YAML tools:** `workspace/tools/*.yaml` — define HTTP API calls with optional response transforms:
- `response_transform: "$.path.to.field"` (JSONPath) — extracts from JSON response
- `auth: { type: bearer_env, key: ENV_VAR }` — field is `type:` NOT `auth_type:` (serde rename)
- `required_base: true` — only available to `base = true` agents
- `channel_action:` — after execution, sends binary result via Telegram (send_photo, send_voice)
- Loaded by `load_yaml_tools(workspace_dir)`, found by `find_yaml_tool(workspace_dir, name)`
- **Conditional SSRF:** `engine_dispatch.rs` picks the HTTP client by endpoint: `tools::ssrf::is_internal_endpoint(&yaml_tool.endpoint)` returns true for trusted admin-configured services (toolgate, browser-renderer, core itself, …) which use the standard `http_client()`. Every other endpoint uses `ssrf_http_client()` with DNS-level private-IP blocking. Path params are URL-encoded, body templates are JSON-escaped. Binary responses limited to 50MB.
- **Tool name validation:** API handlers enforce `[a-zA-Z0-9_-]` on tool and MCP entry names (prevents path traversal)

**Service registry:** `config/services/*.yaml` — infrastructure endpoint definitions (browser-renderer, toolgate). These are infrastructure entries (URL, healthcheck, concurrency), NOT agent tools. Loaded by `service_registry.rs`. Only `toolgate` (fallback for `toolgate_url`) and `browser-renderer` are read by name in the engine. **STT/TTS/embedding/vision are NOT service-registry entries** — those capabilities are resolved through the provider registry (`providers`/`provider_active` tables, UI → Active Providers) and proxied by toolgate.

**Agent skills:** `workspace/skills/*.md` — shared skills for all agents.
`config/skills/*.md` — system skills only available to base agents
(provider-management, channel-management, etc.). Loaded by `skills/mod.rs`.
Skill frontmatter field `pinned: true` protects a skill from the Curator:
Phase 1 never transitions it Active→Stale→Archived, Phase 3 Analyst never
proposes archive/merge/fix. Toggle via `PATCH /api/skills/{name}/pin` or the
lock icon in the Skills UI.

**Agent scaffold:** `crates/opex-core/scaffold/base/` and `scaffold/regular/` — template SOUL.md, IDENTITY.md, HEARTBEAT.md created for new agents. Base agent gets full system template (capabilities, security rules, API reference). Regular agents get lighter template that delegates system tasks to base. Templates use `{AGENT_NAME}` placeholder.

**MCP tools:** external MCP servers run as Docker containers (on-demand via bollard).

### File Handler Hub (toolgate handlers + core orchestration)

File processing (transcribe / describe / extract_document / save / summarize_video /
custom handlers) lives in **toolgate** as self-describing Python handlers
(`toolgate/handlers/builtin/*.py` + `workspace/file_handlers/*.py`, hot-reloaded
via watchfiles). Each handler = an XML descriptor comment + `async def run(ctx, file, params)`.

- **Discovery:** core `agent/handler_registry.rs` (`HandlerRegistry` in `AppState`)
  does a conditional GET of toolgate `GET /handlers` (ETag, ~30s, fail-soft).
- **Matching:** pure-Rust `match_buttons(mime, size, enabled_allowlist, lang)` —
  builtin-tier handlers gated by the GLOBAL `fse.allowlist` (the 5 const ids in
  `FSE_DEFAULT_ALLOWLIST`); workspace-tier allowed by default (trusted-author v1).
- **Run (bytes, never loopback URL):** `gateway/handlers/files.rs` —
  `GET /api/files/{id}/actions` (buttons), `POST /api/files/{id}/run`. Core
  downloads the upload bytes via a loopback signed URL (`mint_uploads_url` +
  `uploads_local_url`) and POSTs **multipart** ("file" + mime/filename/params/
  language) to toolgate `/handlers/{id}/run`; toolgate NEVER fetches the loopback
  URL (mirrors the existing `dispatch.rs` run_transcribe, R12). Sync → inline
  outcome; async → `handler_jobs` row.
- **Async queue:** universal `handler_jobs` table (m067, carries upload_id OR
  source_ref for url-based jobs) + `agent/file_handler_worker.rs`
  (`spawn_file_handler_worker`, 5s poll, stale recovery). The out-of-process
  Python runner reads bytes from a tempfile (no network fetch), posts progress →
  `POST /api/files/jobs/{id}/progress` (WS `file_job_progress`) and the final
  `ScenarioOutcome` → `POST /api/files/jobs/{id}/complete`.
- **Provenance:** `agent/provenance.rs::wrap_file_output` wraps the persisted
  message content (`messages.source='file_handler'`, m066) with
  `<file_output trust="untrusted">` at INSERT time, before it reaches the LLM.

**ctx API** available to all handlers: `stt`, `vision`, `tts`, `imagegen`,
`search`, `embed`, `http`, `result`, `progress`, `llm` (raw-LLM via
`POST /api/llm/complete`). Per-job callback auth uses HMAC `X-Job-Token`.

**Legacy FSE — RETIRED (2026-07-01).** The in-core dispatch/seam/sniffer/rewrite/
owner-gate shell, `gateway/handlers/file_scenarios/run.rs`, the skill-binding
`agent/tool_handlers/file_scenario.rs`, the post-send "file-scenario-chips" SSE
affordance and the Telegram `fse:` callback were all removed. The `file_scenarios`
table is deprecated, not dropped (m069, history-preserving). What survives under
those historical names is **current** toolgate-handler infrastructure, NOT legacy:
`agent/file_scenario/{mod,outcome}.rs` is just the `ScenarioOutcome`/`ScenarioStatus`
wire type parsed by `gateway/handlers/files.rs`, and `agent/fse/allowlist*` is the
GLOBAL builtin-handler allowlist consumed by `agent/handler_registry.rs`. Nothing
here is pending migration.

**Removed in Phase 6:** the in-core async **video** pipeline
(`agent/file_scenario/video_summary.rs`, `video_worker.rs`), the `SummarizeVideo`
dispatch arm + its `EnqueueCtx` plumbing (struct + `DispatchInput.enqueue` field +
`run_builtin` param + seam construction) + the `ScenarioOutcome::video_accepted`
constructor, and the `opex_db::video_jobs` module. Video is now the Python
`summarize_video` async handler on the `handler_jobs` queue. The `video_jobs`
table is deprecated, not dropped (m068, history-preserving). The
`ScenarioOutcome.video_accepted` serde wire field is retained (defaults false).

**Deferred:** untrusted-agent handler isolation (workspace-tier handlers run in-process
as trusted v1); frame/vision descriptions in the video digest.

### Channels (`src/channels/` + `channels/` TypeScript)

In-process channel adapter: `InProcessChannelManager` manages channel lifecycle. TypeScript code in `channels/` runs as a managed child process (NOT Docker). Communication via internal WebSocket loopback.

**Handshake protocol:** On connection, adapter sends `Ready { adapter_type, version, formatting_prompt? }` FIRST. Core replies with `Config { language, owner_id?, typing_mode }`. Adapter MUST wait for `Config` before sending any `Message` events (to receive language preference). See `ChannelInbound` enum doc comment in `crates/opex-types/src/channels.rs` for the canonical sequence.

Channel credentials (`bot_token`, `access_token`, `password`, `app_token`) are extracted from the config on create/update and stored in the encrypted vault under key `CHANNEL_CREDENTIALS`, scope = channel UUID string. The JSONB `config` column in `agent_channels` never contains credential values — they are redacted before DB insert and re-injected from vault on `GET ?reveal=true`.

Agent opts in via TOML: `[agent.channel.telegram] enabled = true`

**Channel WS architecture (post-2026-05-06, plan `2026-05-06-channel-ws-session-correctness.md`):** the per-connection loop in `gateway/handlers/channel_ws/` is split into three concurrent tasks instead of one monolithic `select!`:

- **`reader.rs`** parses `ChannelInbound` and routes by variant. Crucially never awaits engine work — `Message` arriving during processing is no longer silently dropped (the previous bug).
- **`writer.rs`** is the single owner of `ws_sink`, draining `mpsc<OutboundMsg>` from reader / dispatcher / inline / action-forwarder. Eliminates the need for `Arc<Mutex<SplitSink>>`.
- **`dispatcher.rs`** spawns one task per `Message`, serialised by a per-`SessionKey` `tokio::sync::Mutex` from `session_locks.rs`. Different users / chats run concurrently; the same session's messages stay FIFO. `Cancel` for any in-flight `request_id` works (was previously only the foregrounded one).
- **`handshake.rs`** owns the `Ready` exchange + Config reply + pending/outbound replay. Hands off `(channel_type, channel_action_rx)` to the action-forwarder via a oneshot.
- **`inline.rs`** holds non-Message handlers: `Ping`, `AccessCheck`, `Pairing*`, approval-callback intercept.
- **`session_locks.rs`** is a `DashMap<SessionKey, Arc<Mutex<()>>>` with refcount-based eviction (`LockHandle::Drop` releases the guard before counting refs — the `_guard` placement matters).

**Session re-entry:** `get_or_create_session` returns `(Uuid, ReentryMode)`. Soft-terminal sessions (`failed`/`interrupted`/`timeout`/`cancelled`) within the 4-hour window are NOT reused — a fresh session is created instead. `done` sessions are reused (chat continuity). The bootstrap then calls `claim_session_with_retry` which retries once with `ExplicitResume` if a TOCTOU race flipped the status between resolve and claim. `LoopDetector` warm-up from session timeline only runs for `ResumeRunning` (true crash recovery) and `ExplicitResume` (UI-explicit reopen) — `NewSession` and `NewTurnAfterDone` get a fresh detector so prior tool errors don't pollute the next turn.

**Cron mirror:** `mirror_to_session` uses `resolve_active_dm_session` so mirrors land in the same session a live Telegram message would land in — soft-terminal and >4h-stale sessions are skipped.

### Memory (`src/memory.rs`)

PostgreSQL pgvector. Hybrid search: semantic (halfvec) + FTS. MMR reranking. Two tiers: raw (time-decay) + pinned permanent. Embedding is delegated to Toolgate (`POST /v1/embeddings`), which proxies to the configured embedding backend via the `providers` table. Core never calls Ollama or any embedding service directly. Config: `[memory]` section in `opex.toml` — no `embed_url`/`embed_model` keys (those are managed through the providers registry). `embed_dim` is auto-detected at startup.

**Soul layer (autobiographical memory, opt-in via `[agent.soul]`):** `memory_chunks` carries three soul columns — `kind` (`fact` | `event` | `reflection`), `importance`, `lineage` (uuid[] provenance of which chunks a reflection was derived from). Generic memory search/write/decay/hard-delete paths filter on `kind='fact'` so an agent's biography (`event`/`reflection` rows) never surfaces through, or is purged by, the plain `memory` tool — this exemption is applied consistently across all four hard-delete paths (`run_memory_decay`'s low-score sweep, `run_memory_decay_cleanup`'s 180-day sweep, the memory-worker's `clear_existing` reindex purge, and `clear_embeddings`' dim-change wipe — which NULLs soul embeddings instead of deleting the rows). Separately, the agent-facing `memory(delete)` tool and every UI/API mutation route (`api_patch_memory`, `api_patch_document`, `api_delete_memory` — the latter two are the `/api/memory/documents/{id}` routes the Memory page actually calls) are fail-closed guards (shared `refuse_if_biography` helper) that REFUSE to touch `event`/`reflection` chunks; these are immutability/spoofing protection, not hard-delete paths. Deliberate biography removal (quarantine) is done ONLY via the raw-SQL runbook (`docs/runbooks/soul-quarantine.md`), which bypasses these handlers. `event` rows are written by the knowledge extractor as it processes a finished session (source tagged `soul_event:{session_id}`); `reflection` rows are produced by an in-core reflection cycle (no LLM-authored file writes) that periodically summarizes recent events into SELF.md, recording `lineage` back to the events/reflections it consumed. Retrieval for the soul context block (`soul_retrieve`) scores candidates by recency × importance × relevance, with recency computed from `created_at` (not `accessed_at`, unlike the decay-tier scoring above). The `agent_emotion_state` table (migration 083) holds the agent's current mood (valence + label), updated by the knowledge extractor after each session's emotion appraisal (OCC model: valence, intensity, agency, novelty, controllability, desirability, likelihood). Emotion appraisal is gated by `emotion.enabled` (default false); coping (strategy selection from appraisal) is gated by `emotion.coping` (default false); coping biases the reflection threshold only for negative-valence, high-intensity states. ECP (Egocentric Context Projection) reframes recent user turns to separate interlocutor perspective from agent identity, gated by `drift.ecp` (default false). Emotion mood rendering into the prompt is gated by `emotion.render_to_prompt` (default false). All emotion/drift config is per-agent in TOML, all soul state is inspectable via DB rows and timeline events.

**Text normalization SoT:** TTS-specific normalization (numbers→words, English→Cyrillic transliteration) lives in `toolgate/normalize.py` and is **NOT** reused for indexing — it is destructive for embedding/search by design.

**`MEMORY.md` vs `memory_chunks`:** complementary, not redundant.

- `workspace/agents/{Agent}/MEMORY.md` (and other workspace `.md`/`.txt` files) — **hand-edited agent state**, the canonical source of truth. Lives in git-friendly text files. Agents read it on every session start.
- `memory_chunks` (PostgreSQL + pgvector) — **searchable index** of the same content plus runtime knowledge (session summaries, extracted facts). Powers hybrid semantic+FTS search.
- Sync is one-way (file → DB) and event-driven: [memory/watcher.rs](crates/opex-core/src/memory/watcher.rs) listens for workspace file `Create`/`Modify` events and re-indexes the changed file as `scope='shared'` by calling `MemoryStore::index` **directly** (synchronous embedding HTTP call + chunk upsert in the watcher's tokio task). It does NOT go through the `memory_tasks` queue. Editing `memory_chunks` directly never updates `MEMORY.md`.
- **Caveat — per-agent `MEMORY.md` is NOT watched:** the watcher and the reindex file-walk both skip any path under an excluded top-level directory, and `MEMORY_INDEX_EXCLUDE_DIRS` (`crates/opex-core/src/agent/workspace.rs`) includes `"agents"`. Since per-agent state lives at `workspace/agents/{Agent}/MEMORY.md`, editing it does **NOT** reindex into `memory_chunks` — only root-level `workspace/*.md`/`.txt` files (outside the excluded dirs) get the "editing the file updates `memory_chunks`" behavior. Use `POST /api/memory/reindex` or the `memory.reindex` agent action to pick up agent-scoped file edits.
- **First-run bootstrap:** the watcher is delta-only (no initial scan). On startup, if `memory_chunks` has zero `scope='shared'` rows, [main.rs](crates/opex-core/src/main.rs) enqueues a one-shot reindex task into `memory_tasks` (after a toolgate-readiness probe) so workspace files get indexed without manual intervention. This bootstrap path AND the explicit `POST /api/memory/reindex` endpoint are the only two routes that use the memory-worker queue; live file edits skip it. Subsequent restarts skip the bootstrap. Operator can re-trigger anytime via the `memory.reindex` agent action.

### Secrets (`src/secrets.rs`)

ChaCha20Poly1305 encryption, stored in `secrets` table. Resolution order: `(name, scope)` → `(name, "")` global → env var.

### Notifications (`src/gateway/handlers/notifications.rs` + `src/db/notifications.rs`)

PostgreSQL-backed notification system with real-time WebSocket broadcast. Notifications created via `notify(db, ui_event_tx, type, title, body, data)` — persists to DB + broadcasts to all WS clients.

**Triggers:** `access_request` (pairing code), `tool_approval` (agent needs approval), `agent_error` (run failed), `watchdog_alert` (service down).

**API:** `GET /api/notifications` (list + unread_count), `PATCH /api/notifications/{id}` (mark read), `POST /api/notifications/read-all`, `DELETE /api/notifications/clear`.

**UI:** Bell icon in sidebar footer with badge counter, dropdown list, click navigates to relevant page, sound on new notification.

**Note:** Backend serializes `notification_type` field as `"type"` in JSON (serde rename). Frontend `NotificationRow.type` matches this.

### Network Discovery (`src/gateway/handlers/network.rs`)

`GET /api/network/addresses` returns WAN IP (with CGNAT detection), Tailscale status, LAN interfaces, mDNS hostname (`opex.local`). WAN IP cached for 5 minutes. mDNS registered at startup via `mdns-sd` crate.

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
| `"sync_begin"` | envelope open | `boundaryMessageId?`, `runStatus`, `truncated` |
| `"sync_end"` | envelope close | `lastSeq?` |
| `"finish"` | stream end | — |
| `"error"` | stream error | `errorText` |

File events: tool handlers emit `FILE_PREFIX = "__file__:"` inline in tool result; `save_binary_to_uploads()` inserts a row into the `uploads` table (owner_type='tool_output') and returns a signed `/api/uploads/{id}?sig=&exp=` URL. The `/api/uploads/*` path is excluded from auth middleware (HMAC-signed URLs are the security boundary). Agent icons live in the same table (owner_type='agent_icon', permanent); chat composer attachments use owner_type='client_upload'.

## Configuration

**Main config:** `config/opex.toml` — server, DB, embedding, managed processes.

**Agent config:** `config/agents/{Name}.toml` — case-sensitive filename matches agent name.

**Environment:** `.env` in binary dir (auto-loaded). **Policy: only 3 keys belong in `.env`:**

- `OPEX_AUTH_TOKEN` — HTTP API auth token
- `OPEX_MASTER_KEY` — vault encryption key
- `DATABASE_URL` — PostgreSQL connection string

All other configuration (service URLs, API keys, tokens) must go into the secrets vault or `config/opex.toml`. Never add extra keys to `.env`.

## Frontend (`ui/`)

Next.js 16 App Router, React 19, Tailwind 4, shadcn/ui, Zustand state, CodeMirror editor.

RSC chunks are flattened automatically during `next build` via `ui/build/adapter.cjs` (registered in `next.config.ts` as `experimental.adapterPath`). No separate post-build script needed.

**Key stores:** `chat-store.ts` (74 lines — core state machine; actions decomposed into `chat/actions/` — composer, navigation, session-crud, stream-control), `auth-store.ts` (health check + agent list), `canvas-store.ts` (workspace canvas state).

**Chat store decomposition** (Phase 54): `chat-store.ts` was 1891 lines, now split into:

- `chat-types.ts` — types (ChatMessage, MessagePart, AgentState, ConnectionPhase, MessageSource)
- `chat-history.ts` — convertHistory, resolveActivePath, findSiblings, getCachedRawMessages
- `chat-overlay-dedup.ts` — mergeLiveOverlay (dedups live SSE overlay against persisted history)
- `chat-persistence.ts` — saveLastSession, getLastSessionId, getInitialAgent (localStorage)
- `streaming-renderer.ts` — factory via `createStreamingRenderer()`: SSE parsing, rAF throttling (50ms), reconnection, per-agent Map cleanup. Non-serializable state (AbortController, setTimeout) in private closures, not Immer

**Chat components** (`ui/src/components/chat/`): ApprovalCard, ApprovalCountdown, ApprovalArgsEditor, CompressionDivider, AgentTransitionDivider, ReconnectingIndicator, ToolCallPartView

**Utilities:** `card-registry.tsx` (CARD_REGISTRY + GenerativeUISlot + CardErrorBoundary), `citation-tooltip.tsx` (footnote tooltips), `tool-state.ts` (ToolPartState mapper), `use-smoothed-text.ts` (adaptive text streaming animation)

**API types:** `ui/src/types/api.ts` — keep `AgentInfo`, `WebhookEntry`, `ApprovalEntry` etc. in sync with backend JSON responses. SSE event types in `ui/src/stores/sse-events.ts`.

## Database

PostgreSQL 17 + pgvector. Migrations in `migrations/` (sqlx). Auto-run on startup. No ORM — raw sqlx queries in `src/db/`.

Key tables: `sessions`, `messages`, `session_timeline` (chronological lifecycle log), `memory_chunks`, `scheduled_jobs`, `secrets`, `agent_channels`, `usage_log`, `providers`, `provider_active`, `watchdog_settings`.

**Message branching (m012):** `parent_message_id` links to predecessor, `branch_from_message_id` marks fork points. Both nullable — NULL = trunk. Enables conversation tree navigation.

**Session timeline (m013, renamed by m049):** `session_timeline` is a chronological log of session lifecycle events (running, tool_start, tool_end, done, failed, interrupted). Used for LoopDetector warm-up after restart (preserves loop-break decisions across crashes), diagnostics, audit, and the UI Timeline view. Not a Write-Ahead Log: no replay-based recovery; completed work is preserved by persisted side effects, not event replay.

**Active providers:** `provider_active` maps capabilities (stt, tts, vision, imagegen, embedding) to providers. UI configures active providers via the Active Providers page.

## Process Manager

`src/process_manager/` manages native child processes. **Both `channels` and `toolgate` are native processes, NOT Docker containers.** They run as managed subprocesses spawned by Core at startup, with automatic restart on crash.

- **channels** (`channels/` — TypeScript/Bun): Telegram/Discord/Matrix/IRC/Slack adapters. Started by Core, communicates via internal WebSocket loopback.
- **toolgate** (`toolgate/` — Python/FastAPI): Media hub (STT, Vision, TTS, ImageGen, Embeddings). Started by Core with `--workers 1 --loop asyncio` (single process, no multiprocessing workers). Key endpoints: `POST /describe-url`, `POST /transcribe-url`, `POST /v1/audio/speech`, `POST /v1/embeddings`.

**Toolgate router gotcha (Starlette BaseHTTPMiddleware):** `app.py` registers two `@app.middleware("http")` decorators (auth + logging). In Starlette 0.47.x, each creates a `BaseHTTPMiddleware` instance, and two stacked instances corrupt the ASGI receive channel — `request.json()` returns garbled bytes. **All POST routers MUST use `await request.body()` + `json.loads()` instead of `await request.json()`.** See `toolgate/routers/embedding.py` for the pattern.

Config in `config/opex.toml` under `[[managed_process]]`. Restart via `POST /api/services/{name}/restart`. Container restart API has a whitelist — only non-sensitive containers (browser-renderer, searxng, mcp-*) can be restarted; postgres is excluded.

On Pi: `toolgate` source is at `~/opex/toolgate/` (NOT Docker). To deploy toolgate changes: `scp` changed `.py` files to Pi + `POST /api/services/toolgate/restart`. No Docker build needed.

TTS voices: Qwen3-TTS server natively accepts OpenAI voice names (nova, alloy, echo, fable, onyx, shimmer) — no alias mapping in toolgate. Available voices: `GET /v1/audio/voices` on the TTS server. Default voice: `clone:Arty`. TTS server URL is configured via the providers registry.

## Memory Worker (`crates/opex-memory-worker/`)

Separate binary (`opex-memory-worker`) that handles heavy memory tasks asynchronously via a PostgreSQL task queue. Core enqueues tasks; the worker polls and processes them independently.

- Runs as a separate process (own systemd unit or launched alongside core)
- Config: `[memory_worker]` section in `opex.toml` (`enabled`, `poll_interval_secs`)
- Uses `toolgate_url` to call `POST /v1/embeddings` for reindex tasks
- Recovers stuck `'processing'` tasks on startup (crash safety)
- Task types: `reindex` (rebuild embeddings for workspace files)
- Sends `sd_notify` watchdog pings on Linux

## Watchdog Alerting

Watchdog monitors agent inactivity and managed process health. Alert configuration is DB-backed via the `watchdog_settings` table (not in `opex.toml`).

- `GET /api/watchdog/settings` — read current alert settings
- `PUT /api/watchdog/settings` — update settings; allowed keys: `alert_channel_ids`, `alert_events`
- `GET /api/watchdog/status` — current watchdog state per agent
- `GET /api/watchdog/config` / `PUT /api/watchdog/config` — per-agent watchdog config
- Alerts are sent via `POST /api/channels/notify` (body: `{"channel_id": "uuid", "text": "..."}`) — used internally by watchdog and available externally

## Graceful Shutdown

On SIGTERM/SIGINT: drains all running agents (calls `handle.shutdown()` on each), then stops managed processes via `process_manager.stop_all()` (sends SIGTERM to process groups, waits 5s, SIGKILL). Graph worker resets stale 'processing' items on next startup.

<!-- GSD:project-start source:PROJECT.md -->
## Project

**OPEX Stability Audit**

OPEX — Rust-based AI gateway (аналог OpenClaw с более безопасной архитектурой). Единый бинарник обрабатывает HTTP API, жизненный цикл агентов, LLM-вызовы, инструменты, каналы, память и секреты. Проект уже функционирует, цель текущей работы — превентивный аудит и исправление найденных проблем.

**Core Value:** Стабильность и безопасность: найти и устранить баги, несостыковки API, уязвимости и мёртвый код до того, как они проявятся в продакшене.

### Constraints

- **Tech stack**: Rust + rustls-tls only, никакого OpenSSL
- **Deploy target**: x86_64 home-lab server (Pi out of the ecosystem), single binary
- **Backward compat**: исправления не должны ломать API контракты или миграции
<!-- GSD:project-end -->

<!-- GSD:stack-start source:codebase/STACK.md -->
## Technology Stack

## Languages
- Rust 2024 edition - Core application (`crates/opex-core`), type definitions, watchdog, memory worker
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
- Only 3 keys required: `OPEX_AUTH_TOKEN`, `OPEX_MASTER_KEY`, `DATABASE_URL`
- Auto-loaded from binary directory or current working directory
- In production: via systemd `EnvironmentFile=`
- `config/opex.toml` - Server, database, limits, Docker, memory, managed processes
- `config/agents/{Name}.toml` - Individual agent configuration (case-sensitive filename)
- `Makefile` - Build/deploy targets (`make check`, `make test`, `make build-x86_64`, `make remote-deploy`)
- `release.sh` - Multi-architecture release build (aarch64 + x86_64)
- `Cargo.toml` workspace with 4 crates
- `Cargo.lock` - Locked dependency versions
- `docker/docker-compose.yml` - Infrastructure (Postgres, searxng, browser-renderer, MCP servers)
- `docker/Dockerfile.*` - Custom images for Postgres (age, pgvector), sandbox (code execution)
- `ui/package.json` with Next.js build; RSC flattening via `ui/build/adapter.cjs` (`experimental.adapterPath` hook)
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
- Binary deployment: `opex-core`, `opex-watchdog`, `opex-memory-worker`
- Systemd service units (on Linux)
- PostgreSQL 17 + pgvector (managed via docker-compose or external)
- Bun runtime for channel adapters (native process, not Docker)
- Python 3.x runtime for toolgate (native process, not Docker)
- Docker daemon (for MCP containers on-demand, browser-renderer, searxng)
- nginx (for static UI serving post-build)
- Linux x86_64 (home-lab server) - primary deploy target via `make remote-deploy`
- Linux aarch64 - distribution archives only via `release.sh --all` (no longer a deploy target; Pi retired)
- macOS (Intel/Apple Silicon) - cross-compilation untested
- Windows (WSL2 required)
<!-- GSD:stack-end -->

<!-- GSD:conventions-start source:CONVENTIONS.md -->
## Conventions

## Naming Patterns
- React components: PascalCase (e.g., `AgentEditDialog.tsx`, `LanguageToggle.tsx`)
- Utilities and hooks: camelCase (e.g., `api.ts`, `queries.ts`, `chat-store.ts`)
- Rust modules: snake_case (e.g., `json_repair.rs`, `cli_backend.rs`, `tool_registry.rs`)
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
- After the engine refactor (Phase 38+) the engine god-object was decomposed.
  `engine/mod.rs` is now ~600 LoC; tool execution lives in
  `agent/tool_executor.rs`, context building in `agent/context_builder.rs`,
  and the LLM-loop body in `agent/pipeline/execute.rs`.
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
- A single Axum HTTP API (`opex-core`) handles all requests
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
- Location: `crates/opex-core/src/gateway/`
- Contains: Axum router, middleware (auth, CORS, rate limit), SSE event marshalling, handler dispatch
- Depends on: Agent engine, database, config, memory, secrets
- Used by: All external clients (web UI, OpenAI-compatible APIs, channel adapters, webhooks)
- Purpose: Main request handler - calls LLM, parses tool calls, executes tools, streams results
- Location: `crates/opex-core/src/agent/`
- Contains: `engine/` (entry adapters + dispatch, ~600 LoC mod.rs), `pipeline/` (bootstrap/execute/finalize + behaviour layers), `providers/` (LlmProvider trait + 4 impls + factory + routing + http util), tool execution, workspace reading, memory augmentation
- Depends on: LLM providers, tools, workspace, memory, secrets, database
- Used by: Gateway handlers (chat, channel, webhooks)
- Purpose: Execute user-requested operations (workspace edit, web search, code execution, custom HTTP calls)
- Location: `crates/opex-core/src/tools/`
- Contains: YAML tool loader, HTTP client wrappers, SSRF protection, embedding service client
- System tools: registered via `agent/tool_registry.rs` SystemToolRegistry (memory_write, workspace_write, workspace_edit, code_exec, agent, browser_action, etc.)
- YAML tools: loaded from `workspace/tools/*.yaml`, define HTTP API calls with response transforms
- Depends on: Workspace, memory, HTTP client, docker sandbox
- Used by: Agent engine tool execution loop
- Purpose: Store and retrieve contextual information - workspace files, external knowledge, user history
- Location: `crates/opex-core/src/memory.rs`
- Contains: pgvector queries (semantic + FTS), MMR reranking, embedding delegation to Toolgate
- Depends on: PostgreSQL (pgvector), Toolgate (embeddings proxy)
- Used by: Agent engine (build_context)
- Purpose: Persistent storage - sessions, messages, agents, channels, secrets, usage logs, memory chunks
- Location: `crates/opex-core/src/db/` (query functions), `migrations/` (schema)
- Contains: sqlx queries (sessions, messages, approvals, audit, usage, providers, etc.), auto-run migrations on startup
- Tables: sessions, messages, memory_chunks, scheduled_jobs, secrets, agent_channels, usage_log, providers, provider_active, webhooks, approvals, audit_log, and 15+ more
- Depends on: None (consumed by all layers)
- Used by: Gateway, engine, memory, scheduler
- Purpose: File-based state - agent configs, YAML tools, workspace files (memory.md, secrets.md, etc.)
- Location: `crates/opex-core/src/agent/workspace.rs`, `crates/opex-core/src/config/mod.rs`
- Contains: File I/O (read/write protection), hot-reload monitoring, path validation
- Depends on: Filesystem, notify crate (file watcher)
- Used by: Agent engine, tools, gateway
- Purpose: Spawn and supervise long-running native services (Channels TypeScript, Toolgate Python)
- Location: `crates/opex-core/src/process_manager/`
- Contains: Process spawning, restart logic, signal handling, stdio capture
- Services: `channels/` (Telegram/Discord/Matrix/IRC/Slack adapters), `toolgate/` (STT/Vision/TTS/ImageGen/Embeddings)
- Depends on: std::process, tokio
- Used by: Main startup routine
- Purpose: Run code in sandbox and MCP servers on-demand
- Location: `crates/opex-core/src/containers/`
- Contains: bollard Docker client, code sandbox for `code_exec` tool, MCP server launch
- Depends on: Docker daemon, bollard crate
- Used by: Engine (code_exec tool), handlers (MCP startup)
- Purpose: Protect API keys, passwords, credentials (channel bot_token, provider API keys, etc.)
- Location: `crates/opex-core/src/secrets.rs`
- Contains: ChaCha20Poly1305 encryption, scoped resolution (agent + global), env var fallback
- Depends on: PostgreSQL secrets table, chacha20poly1305 crate
- Used by: Agents (resolve auth in YAML tools), gateway handlers (store/retrieve)
- Purpose: Monitor agent inactivity, managed process health, send alerts via channels
- Location: `crates/opex-watchdog/src/main.rs` (separate binary)
- Contains: Cron-based monitoring, alert routing to channels
- Depends on: Database, channel router
- Used by: systemd (separate unit)
- Purpose: Handle heavy async memory tasks (embedding reindex) without blocking core
- Location: `crates/opex-memory-worker/src/main.rs` (separate binary)
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
- Examples: `crates/opex-core/src/agent/providers/{mod,openai,anthropic,google,claude_cli,http,factory,routing,registry}.rs`
- Pattern: Implement `chat()` / `chat_stream()` from the `LlmProvider` trait. Engine calls repeatedly in the LLM-loop (`pipeline::execute`).
- Purpose: Unified representation of all events emitted during message processing (text, tool call, file, error, etc.)
- Examples: `TextDelta("hello")`, `ToolCallStart { id, name }`, `File { url, media_type }`, `Finish { reason }`
- Marshalled to SSE JSON format in `gateway/handlers/chat/sse_converter.rs` for Vercel AI SDK v3 compatibility
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
- Location: `crates/opex-core/src/main.rs` (~43KB), `crates/opex-core/src/gateway/mod.rs`
- Triggers: Startup (`cargo run` or systemd service)
- Responsibilities: Load config, run migrations, spawn agent engines, start process_manager, bind Axum router to port 18789
- Location: `crates/opex-core/src/agent/engine/run.rs` function `handle_sse()` → `pipeline::execute`
- Triggers: POST `/api/chat`, resumed via `/api/chat/{id}/stream`, webhook tools
- Responsibilities: Build context (bootstrap), call LLM (execute), loop tool execution, stream results (sink), persist session (finalize)
- Location: `crates/opex-core/src/agent/pipeline/execute.rs` — tool call dispatch
- Triggers: LLM returns tool_calls in response
- Responsibilities: Dispatch by tool type, handle approval workflow, capture result, continue LLM loop
- Location: `crates/opex-watchdog/src/main.rs`
- Triggers: systemd unit or manual start
- Responsibilities: Poll DB for agent inactivity, send alerts via channel router, restart stale processes
- Location: `crates/opex-memory-worker/src/main.rs`
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
- SSRF: external YAML tool endpoints use `ssrf_http_client()` with a custom DNS resolver that blocks private IP ranges (169.254.x.x, 10.x.x.x, 127.x, 172.16-31.x, 192.168.x); admin-configured internal endpoints recognised by `tools::ssrf::is_internal_endpoint` (toolgate, browser-renderer, …) use the standard client
- API Token: Single bearer token (env var OPEX_AUTH_TOKEN) checked by middleware
- Session ID: Opaque UUID, returned first in SSE (data-session-id event)
- WebSocket Ticket: One-time ticket issued by POST `/api/auth/ws-ticket`, consumed by WS connection
- Channel OAuth: Separate OAuth2 flow in `oauth.rs` for Telegram, Discord, etc.
- Per-minute limit: Configured in `limits.config.max_requests_per_minute` (default 300)
- Tool concurrency: Semaphore in engine limits concurrent tool execution (default 10, configurable)
- Request timeout: Global timeout per request (default 180 seconds, configurable)
<!-- GSD:architecture-end -->
