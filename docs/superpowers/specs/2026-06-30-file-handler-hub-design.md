# File Handler Hub — extracting file processing from the core into self-describing Python handlers

**Date:** 2026-06-30
**Status:** Design approved, ready for implementation plan
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
  `/extract-document`); the async video pipeline runs a durable Postgres queue
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
| 4 | Host for handlers | **Extend toolgate** with a new module (reuses the provider registry + existing endpoints, single process, hot-reload). |
| 5 | Extraction depth | **Full** — all processing logic (incl. async video) moves to Python. The core keeps only what it must own (DB/auth/sessions) + orchestration/security. |
| 6 | Async durable plumbing | A **universal** durable queue in the core (generalize `video_jobs`), not video-specific. Work runs in Python; queue/progress/DB stay in core. |
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
- Security: per-agent `allowlist` (mime → permitted actions), `owner_gate`, and
  **provenance tagging** of processed output before it reaches the LLM (closes the
  multimodal prompt-injection channel flagged by the FSE extensibility deep-research,
  2026-06-24).
- The **universal durable job queue** + WS progress (generalization of `video_jobs`).
- The **discovery cache** of handler manifests (refresh pattern mirrors
  `ProviderRegistry`).
- Routing: composer buttons, channel chips (Telegram `fse:` callback), SSE/WS to chat.

### Data flow — synchronous handler

```
UI: upload file → POST /api/media/upload (existing) → upload_id
UI: GET /api/files/{upload_id}/actions → [buttons]   (core matches mime vs manifest cache + allowlist)
UI: click "Транскрибировать" → POST /api/files/{upload_id}/run {handler_id, params, session, agent}
core: owner_gate + allowlist (re-checked server-side) → toolgate POST /handlers/transcribe/run {signed_url, mime, params}
toolgate: run(ctx, file, params) → ctx.stt.transcribe(...) [provider connection] → {status, summary_text, artifact}
core: persist → provenance-wrap summary_text → SSE file/card to chat + inject as agent context for next turn
```

### Data flow — asynchronous handler (e.g. video)

Same `run` contract, but the descriptor marks `execution=async`:

```
core: enqueue handler_jobs row → return "accepted" ack (persisted as reply, LLM loop short-circuited)
core worker: poll handler_jobs → toolgate POST /handlers/{id}/run {…, job_id}  → 202 Accepted (returns immediately)
toolgate (bg task): run streams ctx.progress(phase, pct) → POST /api/files/jobs/{job_id}/progress (internal callback)
core: update job + emit WS file_job_progress {job_id, handler_id, phase, pct, status}
toolgate (bg task): final ScenarioOutcome → POST /api/files/jobs/{job_id}/complete → core: persist + provenance + agent context
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
  call active providers via toolgate's registry (reuses connections; satisfies
  decision #2).
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
registered). A workspace handler may **not** reuse a built-in `id` — built-in names are
reserved so an agent cannot shadow a trusted handler.

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
  `id → (descriptor, run_fn, source_tier)`.
- `context.py` — builds `ctx`: `ctx.stt/vision/...` wrappers over the existing
  `ProviderRegistry`; `ctx.http` (SSRF-safe); `ctx.result`; `ctx.progress`.
- `watcher.py` — `watchdog` observer on `workspace/file_handlers/` (create/modify/delete)
  → incremental single-file reload, debounced; a parse error does not crash the process.
- `router.py` — FastAPI routes.
- `builtin/` — the 5 ported handlers.

**Workspace path:** the core passes an absolute `workspace_dir` in `/api/media-config`
so toolgate does not guess.

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
- Downloads the file via the HMAC-signed `signed_url` (core-issued `/api/uploads/{id}`;
  treated as an internal endpoint). Download size capped (50 MB, or descriptor
  `max_size_mb`); over-limit → `too_large`.
- Calls `run(ctx, file, params)` under a per-execution timeout.
- Returns a `ScenarioOutcome`-shaped JSON: `{status, summary_text, artifact_urls |
  artifact_b64, reason}`. `status ∈ {ok, failed, unsupported, too_large, timeout}` — the
  **same serde schema the core uses today** (wire-compatible).
- For `execution=async`: accepts `job_id`, streams progress to the core callback, final
  result via the same response (or a final callback).

`GET /handlers/{id}` — a single manifest (debug / UI editor).

**Concurrency:** toolgate stays `--workers 1 --loop asyncio`; handlers are async,
concurrency via the event loop; heavy CPU work is pushed to the provider/subprocess (as
video already does).

### 3. Core orchestration

**`HandlerRegistry` (in `AppState`):** periodic conditional GET of toolgate `/handlers`
by ETag + on-demand refresh. Serves the last cache if toolgate is down (fail-soft for
buttons).

**Matching (pure Rust function):** `(mime, size, agent) → Vec<Button>`: filter manifests
by `match` (mime-glob + `max_size_mb`) → intersect with the per-agent `allowlist` (stays
in core, `fse/allowlist.rs` + `seeder.rs`) → owner-gate → localize `label`. This is why
"each file gets its own buttons".

**Endpoints:**
- `GET /api/files/{upload_id}/actions?agent=&session=` → `{buttons:[{id,label,icon,params}]}`.
  Reads `mime/size` from the `uploads` row, verifies ownership, matches, applies
  allowlist.
- `POST /api/files/{upload_id}/run` → `{handler_id, params, session_id, agent}`:
  - owner-gate + allowlist (**re-checked server-side** — buttons are not trusted).
  - **sync:** call toolgate `/handlers/{id}/run` → `ScenarioOutcome` → persist in
    `uploads`/`messages` → provenance-wrap → SSE `file`/`card` to chat + inject as agent
    context.
  - **async:** enqueue a `handler_jobs` row → return an `accepted` ack (persisted as the
    reply; LLM loop short-circuited — generalization of today's `video_accepted`).

**Provenance wrapper (closes the injection channel):** before reaching the LLM, processed
output is wrapped with a delimiter carrying metadata, e.g.

```
<file_output handler="transcribe" upload="…" trust="untrusted">
…transcript…
</file_output>
```

Signals the model that this is **data from a file, not instructions.** Insertion point:
the bootstrap/context-injection path (as the deep-research recommended — "provenance tag
before the LLM").

### 4. Universal durable async queue (generalization of `video_jobs`)

- New table `handler_jobs` (migration): `id, upload_id, handler_id, agent, session_id,
  params (jsonb), status (queued|processing|done|failed), phase, pct, result (jsonb),
  attempts, created_at, updated_at`. `video_jobs` is cut over to it (video jobs are
  ephemeral — no data migration needed).
- **Core worker** (generalization of `video_worker.rs`): polls `handler_jobs`; on
  `queued` calls toolgate `/handlers/{id}/run` with `job_id`. Resets stale `processing`
  rows on startup (like the memory/graph worker).
- **Progress callback:** toolgate `POST /api/files/jobs/{job_id}/progress {phase, pct}`
  (internal endpoint) → core updates the job → emits a generic WS event
  `file_job_progress {job_id, handler_id, phase, pct, status}` (generalization of
  `video_progress`). The UI progress indicator generalizes accordingly.
- **Finalize:** toolgate returns the `ScenarioOutcome` (or a final callback) → persist +
  provenance + agent reply, as in the sync path.

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
- Buttons are **optional** — the user can still just send the file with text and let the
  agent decide (the old path is preserved).
- Editing `workspace/file_handlers/*.py` reuses the existing Obsidian/CM6 workspace
  viewer tab; no dedicated page in v1.

## Migration & parity

- Port the 5 built-ins to `toolgate/handlers/builtin/*.py`: `save`, `transcribe`,
  `describe`, `extract_document`, `summarize_video` (`execution=async`).
- Behavioural parity: same toolgate calls (`/transcribe`, `/describe-url`,
  `/extract-document`), same `ScenarioOutcome` shape, same `FSE_DEFAULT_ALLOWLIST`.
- Once parity is confirmed, delete the in-core Rust dispatch (`dispatch.rs`,
  `dispatch_seam.rs`, `video_summary.rs`, `video_worker.rs`), keeping the orchestration
  shell (the `ScenarioOutcome` contract type, `allowlist`, `owner_gate`, `rewrite`).
- Switch the Telegram path and SSE chips (`build_file_scenario_chips`) onto
  `HandlerRegistry`.

## Phasing

1. **Contract + descriptor.** `HandlerDescriptor` (Python) + XML parser/validator +
   `ScenarioOutcome` wire type. Parsing tests. *Breaks nothing.*
2. **toolgate hub (sync).** loader + `ctx` + `GET /handlers` + `POST /handlers/{id}/run`.
   Port `save/transcribe/describe/extract_document` to builtin. Hot-reload watcher.
3. **Core orchestration (sync).** `HandlerRegistry` + matching + `GET .../actions` +
   `POST .../run` + provenance + persist. Switch SSE chips and Telegram. *Sync parity
   reached here.*
4. **UI composer.** Buttons above input + run + result rendering + localization.
5. **Universal async queue.** `handler_jobs` + worker + progress callback + generic WS +
   port `summarize_video`. Remove the old video plumbing.
6. **Cleanup.** Delete the dead in-core dispatch, final whole-branch review.

## Testing (TDD per project convention)

- **toolgate (pytest):** descriptor parse/validate, fail-closed on duplicate/invalid,
  `ctx` provider mocks, `run` for each builtin, hot-reload.
- **core (cargo):** mime↔buttons matching, allowlist intersection, owner-gate,
  provenance wrapper, `ScenarioOutcome` (de)serialization, job state machine + recovery,
  ETag registry refresh.
- **UI (vitest):** render buttons from `actions`, click→run, async progress, localization.
- **E2E on server:** upload audio → "Транскрибировать" button → transcript in chat +
  agent sees it as context; upload video → async progress → final.

## Security model

- v1 — trusted authors only (human + `base`); `workspace/file_handlers` written via
  `workspace_write`/UI (core's normal path-guard). Built-in `id`s cannot be overridden.
- `ctx.http` is SSRF-safe; download size capped; `signed_url` is HMAC-validated.
- Provenance wrapping closes the multimodal prompt-injection channel.

## Open questions / deferred

- **Untrusted-agent handler isolation** (WASM/microVM, capability-scoped, no ambient
  credentials) — deferred to a future cycle. Per the FSE extensibility deep-research
  (2026-06-24), the security boundary for arbitrary agent-authored code is *execution
  isolation*, not a registry; this is the staged-trust escalation step.
- **Extension-based matching** and a **dedicated handler-editor UI page** — out of v1.
- Whether the universal queue should later subsume other async work (knowledge
  extraction, reindex) — noted, not in scope.
