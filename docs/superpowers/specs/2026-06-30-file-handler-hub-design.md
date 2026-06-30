# File Handler Hub — extracting file processing from the core into self-describing Python handlers

**Date:** 2026-06-30
**Status:** Design approved + revised after multi-agent review (8 findings folded in), ready for implementation plan
**Supersedes (operationally):** the in-core File Scenario Engine (FSE) dispatch layer
(`crates/opex-core/src/agent/file_scenario/` + `agent/fse/`)

## Problem

File processing ("what to do with an uploaded file" — transcribe, describe, extract
text, summarize video) currently lives **inside the Rust core** as the File Scenario
Engine (FSE):

- 5 actions hardcoded in a Rust dispatch table
  (`dispatch.rs`, `dispatch_seam.rs`): `transcribe`, `describe`, `extract_document`,
  `save`, `summarize_video`.
- The actual compute is delegated to toolgate (`/transcribe`, `/describe-url`,
  `/extract-text-url`); the async video pipeline runs a durable Postgres queue
  (`video_jobs`) + an in-core worker (`video_worker.rs`).
- Action "chips" already exist, but only **after** a message is sent (SSE
  `build_file_scenario_chips`) or as Telegram inline buttons (callback
  `fse:<id>:<action>`).

Three goals, treated as **one phased project**:

1. **Extensibility via Python** — handlers (transcribe/describe/custom) should live as
   self-describing Python scripts that can be added **without rebuilding the core**.
2. **Composer UX** — when a file is uploaded, suggestion buttons ("Транскрибировать",
   "Описать", …) appear **above the input box immediately**, per-file-type, before
   the message is sent.
3. **Decouple the core** — move the file-processing *logic* (including the async video
   pipeline) out of Rust core into Python.

## Decisions (locked during brainstorming)

| # | Decision | Choice |
|---|----------|--------|
| 1 | Scope | One phased project (extraction + Python extensibility + composer buttons) |
| 2 | Button click behaviour | **Direct deterministic run** (no LLM) → result shown in chat **and** fed to the agent as context for its next turn. Direct run **reuses the already-configured provider connections.** |
| 3 | Trust model (v1) | **Trusted authors only** — the human user + the `base` agent. Untrusted-agent isolation (WASM/microVM) explicitly deferred. |
| 4 | Host for handlers | **Extend toolgate** with a new module (reuses the provider registry + existing endpoints, single process for the HTTP facade, hot-reload). |
| 5 | Extraction depth | **Full** — all processing logic (incl. async video) moves to Python. The core keeps only what it must own (DB/auth/sessions) + orchestration/security. |
| 6 | Async durable plumbing | A **universal** durable queue in the core (generalize `video_jobs`), not video-specific. Queue/progress/DB stay in core; work runs in Python in an **out-of-process runner** (the toolgate HTTP facade stays single-process). |
| 7 | Handler storage | **Files** — built-ins in toolgate source; user/agent handlers in `workspace/file_handlers/*.py` with **hot-reload**. Edited via `workspace_write`/UI. |

## Architecture

### The core ↔ Python boundary

**Moves to Python (toolgate):**
- All file-processing logic: `save`, `transcribe`, `describe`, `extract_document`,
  `summarize_video`, and any new handler. Each is a self-describing `.py` file.
- Capability→provider resolution at execution time (via `ctx`, registry already in
  toolgate).

**Stays in the core (its boundary — not extracted):**
- DB, auth, sessions, persistence of results into `uploads`/`messages`.
- Security: per-agent `allowlist` (mime → permitted actions) for the builtin tier,
  `owner_gate`, and **provenance tagging** of processed output before it reaches the LLM
  (closes the multimodal prompt-injection channel flagged by the FSE extensibility
  deep-research, 2026-06-24).
- The **universal durable job queue** + WS progress (generalization of `video_jobs`).
- The **discovery cache** of handler manifests (refresh pattern mirrors
  `ProviderRegistry`).
- The **MCP/Obsidian vault write** for video (a genuinely core-bound side effect — see §4).
- Routing: composer buttons, channel chips (Telegram `fse:` callback), SSE/WS to chat.

### Data flow — synchronous handler

```text
UI: upload file → POST /api/media/upload (existing) → upload_id   (upload fully persisted here)
UI: GET /api/files/{upload_id}/actions → [buttons]   (core matches mime vs manifest cache + tiered allowlist)
UI: click "Транскрибировать" → POST /api/files/{upload_id}/run {handler_id, params, session, agent}
core: owner_gate + allowlist (re-checked server-side) → toolgate POST /handlers/transcribe/run {loopback signed_url, mime, params}
toolgate: await run(ctx, file, params) → await ctx.stt.transcribe(...) [provider connection] → {status, summary_text, artifact}
core: persist as a file-derived message → SSE file/card to chat. The agent picks up the
      same persisted message via build_context() on its next turn, where the provenance
      wrapper is applied to the LLM-facing copy. One path, no separate injection, no
      double-processing.
```

### Data flow — asynchronous handler (e.g. video)

Same `run` contract, but the descriptor marks `execution=async`:

```text
core: enqueue handler_jobs row → return "accepted" ack (persisted as reply, LLM loop short-circuited)
core worker: poll handler_jobs → toolgate POST /handlers/{id}/run {…, job_id} → 202 Accepted (returns immediately)
toolgate: spawns an OUT-OF-PROCESS handler-runner subprocess for the job (HTTP event loop stays free)
runner: rebuilds ctx from /api/media-config → run streams ctx.progress(phase, pct) → POST /api/files/jobs/{job_id}/progress
core: update job + emit WS file_job_progress {job_id, handler_id, phase, pct, status}
runner: final ScenarioOutcome → POST /api/files/jobs/{job_id}/complete → core: persist (file-derived) + agent context
```

## Component design

### 1. Handler model & XML descriptor

One handler = one `.py` file: an XML descriptor block in a top comment (parsed by the
loader), then `async def run`.

```python
# <handler>
#   <id>transcribe</id>
#   <label lang="ru">Транскрибировать</label>
#   <label lang="en">Transcribe</label>
#   <description lang="ru">Речь из аудио/видео в текст</description>
#   <icon>mic</icon>
#   <match>
#     <mime>audio/*</mime>
#     <mime>video/*</mime>
#     <max_size_mb>200</max_size_mb>
#   </match>
#   <capability>stt</capability>          <!-- optional: surfaces active provider name in label -->
#   <execution>sync</execution>           <!-- sync | async -->
#   <output>text</output>                 <!-- text | file | card -->
#   <params>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <order>10</order>                      <!-- button order -->
#   <enabled>true</enabled>
# </handler>

async def run(ctx, file, params):
    text = await ctx.stt.transcribe(file.bytes, language=params.get("language", "ru"))
    return ctx.result.text(text)
```

**`ctx` — the only sanctioned API (provider keys never exposed):**
- `ctx.stt` / `ctx.vision` / `ctx.tts` / `ctx.imagegen` / `ctx.search` / `ctx.embed` —
  **async** wrappers that call active providers via toolgate's `ProviderRegistry`
  (reuses connections; satisfies decision #2). The underlying provider Protocols take the
  shared `httpx.AsyncClient` as their first argument
  (`async transcribe(http, audio_bytes, …)`); the `ctx` wrapper **injects that client
  internally** so the handler calls `await ctx.stt.transcribe(file.bytes, language=…)`
  and never sees the client or credentials.
- `ctx.http` — SSRF-safe HTTP client (mirrors core's `ssrf_http_client`).
- `ctx.result.text(...)` / `.file(bytes, mime)` / `.card(type, data)` — build the
  `ScenarioOutcome`.
- `ctx.progress(phase, pct)` — progress for `execution=async` (no-op for sync).
- `ctx.log` — logging.
- `file` — `{bytes, mime, filename, size, signed_url}`.

**Two tiers:**
- Built-in, trusted, always present: `toolgate/handlers/builtin/*.py` (the 5 ported).
- User/agent-authored: `workspace/file_handlers/*.py` — hot-reloaded (toolgate is a
  native process on the same host, sees `~/opex/workspace`).

**Descriptor validation (fail-closed):** required `id` (unique, `[a-z0-9_-]`), `label`,
≥1 `match` rule, `execution`. Invalid/conflicting files are rejected (logged, not
registered). A workspace handler may **not** reuse a built-in `id` (or another
workspace `id`) — built-in names are reserved (builtin wins) so an agent cannot shadow a
trusted handler; the collision is rejected with a logged error, not a silent drop.

**Match format (v1):** mime-glob + `max_size_mb`. Extension matching (`.srt`) is **not**
included in v1 (mime is sufficient; less duplicate logic).

**Composer params:** if a handler declares `required="true"` params without a default,
the UI shows a mini popover form. All v1 built-ins have defaults → no form needed; the
mechanism is laid down for the future.

### 2. toolgate handler hub (`toolgate/handlers/`)

- `descriptor.py` — `HandlerDescriptor` dataclass + XML parser/validator (single source
  of truth for the schema).
- `loader.py` — scans `builtin/*.py` + `workspace/file_handlers/*.py`, extracts the XML
  block (regex over the leading comment up to the first non-comment line), parses
  (`xml.etree`), validates, imports the module (`importlib`), captures `run`. Registry:
  `id → (descriptor, run_fn, source_tier)`. **Every per-file load is wrapped in
  `try/except (SyntaxError, ImportError, Exception)`** — a bad workspace file is skipped
  and logged as a warning, never aborting the scan or crashing the process. Module-level
  side effects in a workspace file run at import (accepted under the v1 trusted-author
  model; isolation is the deferred untrusted step).
- `context.py` — builds `ctx`: async `ctx.stt/vision/...` wrappers over the existing
  `ProviderRegistry` (injecting the shared `httpx.AsyncClient`); `ctx.http` (SSRF-safe);
  `ctx.result`; `ctx.progress`.
- `watcher.py` — `watchdog` observer on `workspace/file_handlers/` (create/modify/delete)
  → incremental single-file reload, debounced; a parse/import error is caught and logged,
  the process keeps running with the previous registry for that `id`.
- `runner.py` — the **out-of-process handler-runner** entrypoint (see Concurrency): a
  small script launched per async job; rebuilds `ctx` from `/api/media-config`, runs the
  handler, posts progress/complete callbacks to core.
- `router.py` — FastAPI routes.
- `builtin/` — the 5 ported handlers.

**Workspace path:** the core adds a **new** `workspace_dir` field to the
`/api/media-config` response (today it returns only `version`/`active`/`providers`) so
toolgate gets the absolute workspace path instead of guessing. The toolgate config loader
(`toolgate/config.py`) is extended to read it.

**HTTP endpoints:**

`GET /handlers` → `{ handlers: [HandlerManifest...], etag }` — manifests for discovery.
`provider` is resolved from the top active provider when `capability` is set. ETag lets
the core refresh with a conditional GET (mirrors `ProviderRegistry`, ~30 s).

```json
{
  "id": "transcribe",
  "labels": {"ru": "Транскрибировать", "en": "Transcribe"},
  "descriptions": {"ru": "...", "en": "..."},
  "icon": "mic",
  "match": {"mime": ["audio/*", "video/*"], "max_size_mb": 200},
  "capability": "stt",
  "provider": "speaches-local",
  "execution": "sync",
  "output": "text",
  "params": [{"name": "language", "type": "string", "default": "ru", "required": false}],
  "order": 10,
  "tier": "builtin"
}
```

`POST /handlers/{id}/run` → body `{signed_url, mime, filename, size, params, language, job_id?}`:
- Downloads the file via the HMAC-signed `signed_url`. The core mints a **loopback signed
  URL** for this fetch — base = core's local listen addr (e.g.
  `http://127.0.0.1:18789/api/uploads/{id}?sig=&exp=`), never the public host — so the
  server-side fetch stays on loopback and does not traverse the egress proxy. The HMAC
  payload (`uploads:{id}:{exp}`) is host-agnostic, so the loopback base validates fine.
  Download size capped (50 MB, or descriptor `max_size_mb`); over-limit → `too_large`.
- **sync handlers:** `await run(ctx, file, params)` runs inline under a per-execution
  timeout; a CPU-bound section inside a handler (pymupdf/ffmpeg, which release the GIL) is
  offloaded with `asyncio.to_thread` so it does not block the event loop. Returns a
  `ScenarioOutcome` JSON: `{status, summary_text, artifact_urls | artifact_b64, reason}`,
  `status ∈ {ok, failed, unsupported, too_large, timeout}` — the **same serde schema the
  core uses today** (wire-compatible).
- **async handlers:** the request returns **202 Accepted immediately** and the job runs
  in an **out-of-process handler-runner subprocess** (NOT on the HTTP event loop). The
  runner posts `ctx.progress` to the core progress callback and the final
  `ScenarioOutcome` to the core **complete callback**
  (`POST /api/files/jobs/{job_id}/complete`). Required because the HTTP facade is
  `--workers 1`: an hour-long job must not occupy the single event loop.

`GET /handlers/{id}` — a single manifest (debug / UI editor).

**Concurrency / execution model:** the toolgate HTTP facade stays `--workers 1 --loop
asyncio` (per CLAUDE.md — providers hold in-process state; multi-worker is not adopted).
Threads alone do **not** fix long jobs (the GIL serialises pure-Python CPU, and an
hour-long job sharing the server process is a memory/stability risk). So execution is
split by handler kind:
- **sync handlers** run inline on the loop (mostly `await` to providers); a CPU-bound
  section is offloaded via `asyncio.to_thread` (pymupdf/ffmpeg release the GIL).
- **async handlers** run in a dedicated **out-of-process runner** (`asyncio.create_
  subprocess_exec` of `runner.py`, one process per job). The runner rebuilds `ctx` from
  `/api/media-config`, executes `run`, and posts progress/complete callbacks straight to
  core. The HTTP loop is never blocked, the job is isolated, and a job crash cannot take
  down toolgate.

### 3. Core orchestration

**`HandlerRegistry` (in `AppState`):** periodic conditional GET of toolgate `/handlers`
by ETag + on-demand refresh. Serves the last cache if toolgate is down (fail-soft for
buttons).

**Matching (pure Rust function):** `(mime, size, agent) → Vec<Button>`: filter manifests
by `match` (mime-glob + `max_size_mb`), then apply the trust gate **by tier**:
- **builtin-tier** handlers intersect with the per-agent `allowlist` (stays in core,
  `fse/allowlist.rs` + `seeder.rs`) — preserves today's behaviour.
- **workspace-tier** handlers are **allowed by default** (gated only by `owner_gate` +
  mime match), since v1 authors are trusted (decision #3). This is what makes a freshly
  added `workspace/file_handlers/foo.py` actually appear as a button — without it, the
  fixed `FSE_DEFAULT_ALLOWLIST` seed would silently hide every custom handler, defeating
  goal #1.

Then `owner_gate` → localize `label`. This is why "each file gets its own buttons".
Per-agent enable/disable of workspace handlers (an allowlist-management UI) is a future
extension; v1 is default-on for trusted authors.

**Endpoints:**
- `GET /api/files/{upload_id}/actions?agent=&session=` → `{buttons:[{id,label,icon,params}]}`.
  Reads `mime/size` from the (already-persisted) `uploads` row, verifies ownership,
  matches, applies the tiered gate.
- `POST /api/files/{upload_id}/run` → `{handler_id, params, session_id, agent}`:
  - owner-gate + tiered allowlist (**re-checked server-side** — buttons are not trusted).
  - **sync:** call toolgate `/handlers/{id}/run` → `ScenarioOutcome` → persist as a
    file-derived message in `uploads`/`messages` → SSE `file`/`card` to chat. The agent
    consumes it via `build_context()` (provenance applied there).
  - **async:** enqueue a `handler_jobs` row → return an `accepted` ack (persisted as the
    reply; LLM loop short-circuited — generalization of today's `video_accepted`).

**Provenance wrapper (closes the injection channel):** the LLM-facing copy of a
file-derived message is wrapped with a delimiter carrying metadata, e.g.

```text
<file_output handler="transcribe" upload="…" trust="untrusted">
…transcript…
</file_output>
```

Signals the model that this is **data from a file, not instructions.** Insertion point:
the result is persisted as a normal message **tagged as file-derived** (source/metadata
flag); `pipeline::bootstrap::build_context()` applies the wrapper to file-derived message
content **when assembling the LLM input** on the agent's next turn. The chat UI renders
the clean message; only the LLM-facing copy is wrapped. One path, no double-processing
(as the deep-research recommended — "provenance tag before the LLM").

### 4. Universal durable async queue (generalization of `video_jobs`)

- New table `handler_jobs` (migration): `id, upload_id, handler_id, agent, session_id,
  params (jsonb), status (queued|processing|done|failed), phase, pct, result (jsonb),
  attempts, created_at, updated_at`. `video_jobs` is cut over to it (video jobs are
  ephemeral — no data migration needed).
- **Core worker** (generalization of `video_worker.rs`): polls `handler_jobs`; on
  `queued` calls toolgate `/handlers/{id}/run` with `job_id` (which spawns the
  out-of-process runner). Resets stale `processing` rows on startup (like the
  memory/graph worker).
- **Progress callback:** runner `POST /api/files/jobs/{job_id}/progress {phase, pct}`
  (internal endpoint) → core updates the job → emits a generic WS event
  `file_job_progress {job_id, handler_id, phase, pct, status}` (generalization of
  `video_progress`). The UI progress indicator generalizes accordingly.
- **Finalize:** the runner posts the final `ScenarioOutcome` to
  `POST /api/files/jobs/{job_id}/complete` → core persists (file-derived) + delivers the
  agent reply, as in the sync path.

**Generic schema, explicit video boundary.** `handler_jobs.params` and `.result` are
JSONB catch-alls — the queue itself stays handler-agnostic (no video columns). The
video-specific *media processing* (timestamp chunking, map-reduce digest, slug
generation, markdown/frontmatter assembly) moves into the Python `summarize_video`
handler — it is all just Python there. The **Obsidian-vault note write via MCP**, which
today lives in `video_worker.rs`, is **not** reimplemented in Python: it stays a
core-side post-completion step, reusing the existing core MCP plumbing, triggered when the
outcome's `result` JSON requests it (a generic `post_action` field). So the queue
generalizes cleanly and video is not special-cased *in the queue*, while the one
genuinely core-bound side effect (MCP vault write) stays in core.

### 5. UI composer (`ChatComposer.tsx`)

- After `handleFileAdd` (upload → `upload_id`): `GET /api/files/{upload_id}/actions` for
  the current agent/session. Buttons render **above the input**, grouped per attachment
  (each file has its own). State lives in the composer's local state (not `chat-store`).
- Click → `POST /api/files/{upload_id}/run` → inline spinner on the button → the result
  arrives through the normal path into the chat (SSE `file`/`card` for sync; WS
  `file_job_progress` + final for async).
- Handlers with `required` params (no default) → mini popover with fields (mechanism laid
  down; unused by v1 built-ins).
- Localization: `labels[lang]` from the manifest + `language-store`.
- Buttons are **optional** — the upload is already persisted by `POST /api/media/upload`
  before `actions` is fetched (no race), so not clicking any button just leaves the file
  as a normal attachment to send with text and let the agent decide (the old path is
  preserved).
- Editing `workspace/file_handlers/*.py` reuses the existing Obsidian/CM6 workspace
  viewer tab; no dedicated page in v1.

## Migration & parity

- Port the 5 built-ins to `toolgate/handlers/builtin/*.py`: `save`, `transcribe`,
  `describe`, `extract_document`, `summarize_video` (`execution=async`).
- Behavioural parity: same toolgate calls (`/transcribe`, `/describe-url`, and
  **`/extract-text-url`** — the real endpoint name is `extract-text-url`, not
  `extract-document`), same `ScenarioOutcome` shape, same `FSE_DEFAULT_ALLOWLIST` for the
  builtin tier.
- Once parity is confirmed, delete the in-core Rust dispatch (`dispatch.rs`,
  `dispatch_seam.rs`, `video_summary.rs`, `video_worker.rs`), keeping the orchestration
  shell (the `ScenarioOutcome` contract type, `allowlist`, `owner_gate`, `rewrite`).
- Switch the Telegram path and SSE chips (`build_file_scenario_chips`) onto
  `HandlerRegistry`.

## Phasing

1. **Contract + descriptor.** `HandlerDescriptor` (Python) + XML parser/validator +
   `ScenarioOutcome` wire type. Parsing tests. *Breaks nothing.*
2. **toolgate hub (sync).** loader + `ctx` + `GET /handlers` + `POST /handlers/{id}/run`
   (sync path only) + `workspace_dir` added to `/api/media-config`. Port
   `save/transcribe/describe/extract_document` to builtin. Hot-reload watcher + import
   error boundary.
3. **Core orchestration (sync).** `HandlerRegistry` + tiered matching +
   `GET .../actions` + `POST .../run` (sync) + provenance in `build_context()` + persist.
   Switch SSE chips and Telegram **for sync handlers only**. **`summarize_video` keeps
   routing through the existing in-core `video_jobs` dispatch** — untouched until Phase 5,
   so async video never regresses. *Sync parity reached here.*
4. **UI composer.** Buttons above input + run + result rendering + localization. (Video
   still served by the legacy async path; its button routes to the legacy dispatch.)
5. **Universal async queue.** `handler_jobs` + core worker + out-of-process runner +
   progress/complete callbacks + generic WS. Port `summarize_video` to a Python async
   handler; cut its routing from the legacy `video_jobs` path to the new queue **in the
   same phase** (no window where both/neither own video). MCP vault write kept as the
   core post-completion step.
6. **Cleanup.** Once the new async path owns video, delete the dead in-core dispatch
   (`dispatch.rs`, `dispatch_seam.rs`, `video_summary.rs`, `video_worker.rs`, `video_jobs`
   plumbing) and run a final whole-branch review.

## Testing (TDD per project convention)

- **toolgate (pytest):** descriptor parse/validate, fail-closed on duplicate/invalid id,
  import error boundary (syntax-error file is skipped, process survives), `ctx` provider
  mocks (async + injected client), `run` for each builtin, hot-reload, out-of-process
  runner progress/complete callbacks.
- **core (cargo):** tiered mime↔buttons matching (builtin∩allowlist, workspace default-on),
  owner-gate, provenance wrapper applied in `build_context()` for file-derived messages,
  loopback signed-URL minting, `ScenarioOutcome` (de)serialization, `handler_jobs` state
  machine + stale-recovery, ETag registry refresh.
- **UI (vitest):** render buttons from `actions`, click→run, async progress, localization.
- **E2E on server:** upload audio → "Транскрибировать" button → transcript in chat +
  agent sees it as context; add a custom `workspace/file_handlers/*.py` → its button
  appears (extensibility); upload video → async progress → final, vault note written.

## Security model

- v1 — trusted authors only (human + `base`); `workspace/file_handlers` written via
  `workspace_write`/UI (core's normal path-guard). Built-in `id`s cannot be overridden.
- **Workspace-tier handlers are allowed by default** (bypass the per-agent builtin
  allowlist) — this is the trust assumption that makes extensibility work; it holds only
  because v1 authoring is restricted to trusted principals. When untrusted-agent authoring
  is added later, workspace-tier handlers must move behind isolation + an explicit
  per-agent grant (see Open questions).
- `ctx.http` is SSRF-safe; download size capped; the upload `signed_url` is HMAC-validated
  and minted **loopback-only** for the server-side fetch.
- Async jobs run in an out-of-process runner, isolating long/heavy work from the toolgate
  HTTP facade.
- Provenance wrapping (applied in `build_context()` to file-derived messages) closes the
  multimodal prompt-injection channel.

## Open questions / deferred

- **Untrusted-agent handler isolation** (WASM/microVM, capability-scoped, no ambient
  credentials) — deferred to a future cycle. Per the FSE extensibility deep-research
  (2026-06-24), the security boundary for arbitrary agent-authored code is *execution
  isolation*, not a registry; this is the staged-trust escalation step. At that point
  workspace-tier default-on trust (§Security) must be replaced by isolation + per-agent
  grant.
- **Extension-based matching** and a **dedicated handler-editor UI page** — out of v1.
- Whether the universal queue should later subsume other async work (knowledge
  extraction, reindex) — noted, not in scope.
