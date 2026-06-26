# FSE Video Summarization — Design

- **Date:** 2026-06-26
- **Status:** Approved (brainstorm complete) — **rev 1** after a code-fact review + user scoping. Decisions baked in: **web-only for v1** (Telegram delivery deferred); the worker lives in `opex-core` (not memory-worker — it needs LLM providers / `ui_event_tx`); the FSE seam **does** change (session/agent threaded for enqueue); **no artificial limits** — local Whisper transcribes any-length audio (the 25 MB cap is a cloud-API artifact we lift), large-context models take the whole transcript and scene-driven frames with no hard cap. Ready for implementation planning.
- **Related:**
  - [`2026-06-22-file-scenario-engine-design.md`](2026-06-22-file-scenario-engine-design.md) — the FSE foundation this builds on. That spec deferred video summarization as a future plugin (§2 Non-goals, §6 Scope discipline) and assumed it would land as a *skill + `video/*` binding*. **This design deliberately deviates** — see §4.
  - `D:\GIT\telesumbot` — the reference pipeline. A standalone Rust Telegram bot that turns video into a Russian-language multimodal summary. We reuse its *algorithm* (scene-frame extraction params, ASR options, summary prompts), **not** its services.
  - `crates/opex-core/src/agent/file_scenario/dispatch.rs` — the in-core built-in dispatch table this extends (4 → 5 actions).
  - `crates/opex-memory-worker/` — the durable task-queue **pattern** (`memory_tasks` claim/recover) `video_jobs` mirrors. The worker itself lives in `opex-core`, not here (it needs LLM providers, `ui_event_tx`, and channel delivery the memory-worker lacks).
  - `crates/opex-core/src/db/sessions.rs::mirror_to_session(db, agent_id, channel, chat_id, text)` — the verified delivery primitive (used by cron at `scheduler/mod.rs:1229,1481`).
  - `toolgate/routers/stt.py`, `toolgate/routers/vision.py` — the provider-reuse pattern the new `routers/video.py` follows.

---

## 1. Problem

OPEX should turn an inbound **video** into a full multimodal summary (transcript + key-frame descriptions + an LLM-written digest), the way `telesumbot` does — but **without standing up a duplicate stack**. The user confirmed:

- **Desired output:** full summary like telesumbot (transcript + scene key-frames + multimodal LLM summary).
- **Already deployed in OPEX:** STT (toolgate `/transcribe`), Vision (`/describe`), text-LLM providers, and system `ffmpeg`.
- **Not deployed:** PySceneDetect, yt-dlp, and telesumbot itself.
- **Sources:** both uploaded video files **and** links (YouTube etc.).

The FSE foundation already dispatches `audio→transcribe` and `image→describe` deterministically before the LLM, but those are **fast** (seconds, synchronous, 60 s ceiling). A full video summary runs for **minutes** (extract → ASR → vision over N frames → LLM). It cannot run inside the synchronous pre-LLM FSE seam.

## 2. Goals / Non-goals

### Goals

- A `video/*` file scenario that produces a telesumbot-quality summary and delivers it back to the originating session/chat.
- Reuse the **existing** integrations: toolgate STT, toolgate Vision, core text-LLM providers, system ffmpeg.
- Scene-based key-frame extraction **without a new service** — ffmpeg's `select='gt(scene,THRESHOLD)'` filter (threshold seeded from telesumbot's `SCENE_THRESHOLD=27` content detector, mapped to ffmpeg's 0..1 scene score).
- Both inputs: uploaded `video/*` files (FSE path) and video links (URL path) → one pipeline.
- **Asynchronous**: instant ack on intake, durable processing that survives restart, summary delivered when ready.
- Keep `opex-core` thin: heavy media work lives in toolgate; the core owns the trigger, async orchestration, LLM generation, and delivery.

### Non-goals (deferred)

- Real-time / live-stream analysis.
- Progress-stage updates during processing (telesumbot streams stages; v1 ships ack + final only).
- OCR slide-filtering / PaddleOCR (telesumbot optional feature) — not in v1.
- Per-agent scenario scope (FSE is global today).
- Arbitrary-domain video download — v1 yt-dlp allowlist is YouTube only (§9).
- **Telegram (and other channel) delivery — v1 is web-only.** The pipeline and `video_jobs` schema keep `channel_id` for it, but channel delivery lands later.

## 3. Approach (chosen)

**Approach A — toolgate `/summarize-video` + thin core built-in + durable job.** Selected over (B) wrapping telesumbot as a service and (C) porting telesumbot's Rust pipeline into core. Rationale: A reuses the exact integrations already running, avoids a duplicate Whisper/Ollama stack, keeps the core thin, and gets scene detection from ffmpeg without a new service. telesumbot becomes the **reference algorithm**, not a dependency.

## 4. Relationship to the FSE foundation — deliberate deviation

The FSE design (2026-06-22) assumed video would be a **skill + `video/*` binding** (`executor=skill`). We deviate to a **built-in tool (`executor=tool`) + async job**. Why:

1. **Skills never auto-run in FSE.** By FSE rule, `executor=skill` bindings are *never* the 0-click default — they only surface as a selectable alternative. The user wants automatic full-summary on a video arriving.
2. **Skills run inside the agent's LLM loop.** A skill-mediated summary would block the agent/session for the minutes the pipeline takes. A built-in that *enqueues* and returns immediately keeps the session responsive.
3. **Determinism + cost control.** A built-in gives a fixed pipeline, a per-job cost cap (frame count), and an auditable `fse_auto_run`, consistent with the other built-ins.

This makes `summarize_video` the **5th built-in** and adds it to `FSE_DEFAULT_ALLOWLIST`. The deviation is intentional and supersedes the foundation's tentative note for this capability.

## 5. Architecture

Three new units, each with one job:

### 5.1 toolgate `routers/video.py` — `POST /summarize-video`

The media worker. Synchronous from its caller's view (the core worker holds one long HTTP request, read-timeout disabled, as `app.py` already configures `read=None`). Steps:

1. Obtain the video bytes: download the localhost-rewritten upload URL, **or** (URL source) `yt-dlp` fetches the link to a temp file.
2. `ffmpeg` extracts the audio track (single decode pass).
3. `ffmpeg` extracts key frames via `select='gt(scene,THRESHOLD)'`, written as JPEGs — **scene-driven, no fixed frame cap** (large-context Vision/LLM make a hard cap unnecessary; only a high safety ceiling guards pathological inputs).
4. **Audio for STT — no length limit.** The local Whisper provider handles any-length audio (it windows internally). The only barrier is the artificial `STT_MAX_BYTES = 25 MB` check (`stt.py:18`), inherited from the **cloud** OpenAI Whisper API — we **lift it for the local provider** and transcribe whole. Time-chunking remains only as a fallback if a cloud STT provider with a hard per-file limit is ever made active.
5. Transcribe via `require_provider("stt").transcribe(...)` **directly** in-process (the same path `/transcribe` uses) — no HTTP self-hop.
6. Describe each key frame **directly** via the Vision provider (`provider.describe(...)`), bounded concurrency.
7. Return **raw material**, not a finished summary:
   ```json
   {
     "duration": 743.0,
     "transcript": "…",
     "frames": [{"timestamp": 12.4, "description": "…", "image_url": "…"}],
     "degraded": {"vision": false, "stt": false}
   }
   ```

The final LLM digest is **not** done here (toolgate has no text-LLM). This avoids a toolgate→core callback and keeps LLM routing/fallback in the core.

### 5.2 opex-core — built-in `summarize_video` (enqueue-only)

Added to `dispatch.rs` as `BuiltinAction::SummarizeVideo` and to `FSE_DEFAULT_ALLOWLIST`. **Its body is fast:** validate, `INSERT video_jobs (status=pending, …)`, and return an ack outcome (`ScenarioOutcome::ok("🎬 Видео принято, готовлю сводку…")`). The heavy work happens out-of-band in the worker.

**Seam change (review R2):** unlike the other built-ins, enqueue needs `session_id` + `agent_name` for delivery — and these are **not** currently threaded into the FSE seam. `enrich_message_text` (`subagent.rs:250`) and `dispatch_attachments` / `dispatch_action` receive `db` + `attachments` but no session context. So this work threads `session_id` + `agent_name` from `bootstrap.rs` down to the enqueue point (`channel_id` deferred with Telegram, §7). The seam stays synchronous and fast; it just gains the context it needs to record the job.

### 5.3 opex-core — the video worker (in-core, review R1)

A tokio worker **inside `opex-core`**, NOT `opex-memory-worker`. The memory-worker has only `db` + `toolgate_url`; the video worker needs LLM providers (final digest), `ui_event_tx` (live web push) and session delivery — all of which live in core. It reuses the durable-queue *pattern* from memory-tasks (claim/recover), not the binary. Loop:

1. Claim a `pending` job (`UPDATE … status=processing` with a claim guard).
2. `POST toolgate /summarize-video` with the source (long request).
3. Build the final summary via the core's own text providers over the **whole** transcript + frame descriptions — large context means no telesumbot-style 40 k chunking. Prompts ported from telesumbot `summary/prompts.rs`.
4. **Deliver** to the originating web session (§7).
5. `UPDATE video_jobs status=done` (store the summary) or `failed` (store the error).

### 5.4 opex-core — video-URL detector

In `enrich_message_text` (`subagent.rs`), beside `extract_urls`: recognize video links from the **yt-dlp domain allowlist** (§9). A matched link enqueues a `video_jobs` row with `source_type=url` and emits the same ack. File uploads go through the FSE `video/*` binding; both converge on the same queue.

## 6. Data flow

```text
INPUT A: video file (attachment)         INPUT B: video link (YouTube)
   │ FSE: sniff video/*                       │ URL-detect in enrich_message_text
   │ → built-in summarize_video               │ (allowlisted domain)
   ▼                                           ▼
   └────────────────┬──────────────────────────┘
                    ▼
   core: INSERT video_jobs(pending) + instant ack «🎬 Обрабатываю видео…»
                    ▼   (message handling continues; session NOT blocked)
   in-core video worker claims job
                    ▼
   POST toolgate /summarize-video        (minutes; read-timeout disabled)
     download/yt-dlp → ffmpeg(audio+scene-frames) → STT(whole) → Vision(frames)
                    ▼   returns {transcript, frames[], duration, degraded}
   core: LLM digest via text providers, whole transcript (telesumbot prompts)
                    ▼
   deliver to the SAME web session (mirror_to_session + ui_event push)
                    ▼
   UPDATE video_jobs status=done|failed
```

## 7. Delivery (web-only for v1)

The `session_id` + `agent_name` are captured in the `video_jobs` row at intake. On completion the worker:

- Injects the summary as an assistant message into the originating session via `db::sessions::mirror_to_session(...)` (the verified primitive cron uses), so the web client renders it.
- Pushes a live `ui_event_tx` event so an already-open web client shows the summary without a reload.
- On `failed`, delivers a plain notice («Не удалось обработать видео: <причина>») the same way.

**Telegram delivery is deferred** (out of v1). The `channel_id` column exists for it but stays NULL for now; when Telegram lands, the worker adds a `channels/notify` send keyed by `channel_id`.

## 8. Storage

### 8.1 New table `video_jobs` (migration)

```sql
CREATE TABLE video_jobs (
    id           UUID PRIMARY KEY,
    session_id   UUID NOT NULL,
    agent_name   TEXT NOT NULL,
    channel_id   UUID,                       -- always NULL in v1 (web-only); reserved for Telegram
    source_type  TEXT NOT NULL CHECK (source_type IN ('file','url')),
    source_ref   TEXT NOT NULL,              -- upload URL or video link
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending','processing','done','failed')),
    summary      TEXT,                        -- final digest on done
    error        TEXT,                        -- reason on failed
    attempts     INT  NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX video_jobs_claim_idx ON video_jobs (status, created_at);
```

**Startup recovery:** stuck `processing` rows reset to `pending` (crash safety), exactly as the memory worker recovers stuck tasks.

### 8.2 FSE binding (seed)

A new default `SeedRow` in `fse/seeder.rs`: `match_type='video/*', executor='tool', action_ref='summarize_video', label='Сводка видео', is_default=true, priority=100`, plus `summarize_video` added to `FSE_DEFAULT_ALLOWLIST` (and therefore to `dispatch.rs::resolve`). The seeder is idempotent (`ON CONFLICT DO NOTHING`); existing deployments get the row via the same one-shot seed path on next start, or an operator can add it in the UI.

## 9. Configuration & security

**No artificial content limits** (project direction). Large-context models + the local Whisper provider remove the need for caps: **no** video-duration cap, **no** byte-size cap, **no** fixed frame cap (frames are scene-driven), whole transcript to the LLM. The 25 MB STT check is lifted for the local provider (§5.1).

What remains is operational/safety, not throttling:

- **Liveness timeout** per job (generous, configurable, high default) — a guard so a wedged ffmpeg/yt-dlp/provider call eventually fails the job instead of hanging `processing` forever. **Not** a cap on legitimate long videos.
- **Scene threshold** configurable; default ported from telesumbot's `SCENE_THRESHOLD`.
- **High safety ceiling on frame count** — not a product cap; only guards a pathological input (e.g. strobing video → tens of thousands of scene cuts).
- **yt-dlp domain allowlist** — v1 = YouTube only. URL download is an SSRF surface; reuse the `tools::ssrf` posture (block private IPs, only allowlisted hosts). Uploaded files already go through the signed `/api/uploads` path.
- **Config** follows the project rule: tunables in `config/opex.toml` / toolgate config, never new `.env` keys.

## 10. Error handling & degradation

- **No Vision provider** → summarize from transcript only; `degraded.vision=true` noted in the digest.
- **No STT provider** → honest `failed` (a silent-film summary is not the goal).
- **yt-dlp failure** (private/geo-blocked/unavailable) → `failed` with the yt-dlp reason surfaced.
- **ffmpeg failure** (corrupt/unsupported container) → `failed` with a clear message.
- **Toolgate/worker crash mid-job** → startup recovery re-queues; `attempts` bounds retries; on exhaustion → `failed` + user notice.
- **Delivery failure** → the `done` summary is persisted in `video_jobs` regardless; the web client also recovers it on reload (it is a normal session message). Never silently dropped.
- **Audit:** the built-in enqueue emits `fse_auto_run` like the other defaults; the worker logs per-job outcome.

## 11. Testing (TDD)

- **toolgate `video.py`:** short real video fixtures; ffmpeg audio+scene-frame extraction asserted on structure; STT/Vision providers mocked; raw-material JSON shape verified; `max_frames` cap enforced; degraded paths.
- **core built-in:** `summarize_video` enqueues a row and returns the ack **without** calling toolgate (proves non-blocking); unknown-action fail-closed unchanged; `FSE_DEFAULT_ALLOWLIST`/`resolve` parity guard updated for the 5th action.
- **worker:** claims pending → processes → delivers → marks done; durable claim is idempotent; stuck-`processing` recovery on startup; `attempts` retry ceiling → `failed`.
- **FSE seam:** `video/*` resolves to `summarize_video`, returns ack + a `video_jobs` row exists (NOT a synchronous toolgate call).
- **URL detector:** allowlisted video link enqueues a `url` job; non-allowlisted link does not.
- **delivery:** summary lands in the originating web session (`mirror_to_session`) and a `ui_event` is pushed; no Telegram path in v1.

## 12. Resolved defaults

- delivery = **web-only** for v1 (Telegram deferred).
- worker = **in-core tokio worker** over durable `video_jobs` (memory-worker untouched).
- limits = **none artificial** — any-length video, whole-transcript LLM, scene-driven frames; only a liveness timeout + SSRF allowlist remain.
- yt-dlp allowlist = **YouTube only** for v1.
- final LLM digest = **core** (text providers), toolgate returns raw material only.

## 13. Open questions (for the plan, not blockers)

- Exact mapping of telesumbot's `SCENE_THRESHOLD=27` (content detector) to ffmpeg's 0..1 `scene` score — calibrate against fixtures during implementation.
- Whether to also expose `summarize_video` as an agent-callable tool (so an agent can summarize a video on demand, not only via auto-dispatch). Likely yes, cheap once the built-in exists; confirm in planning.
- Progress-stage updates (telesumbot streams stages) — deferred; revisit if users want feedback during the minutes-long wait.
- Worker concurrency — how many video jobs run at once (ffmpeg + STT + Vision are resource-heavy; telesumbot serializes with a GPU cooldown). Start with 1 concurrent job; revisit.
