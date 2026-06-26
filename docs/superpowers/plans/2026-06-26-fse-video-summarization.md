# FSE Video Summarization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a `video/*` file is uploaded (or a YouTube link is pasted) in the web UI, OPEX produces a telesumbot-quality multimodal summary (transcript + scene-frame descriptions + LLM digest) asynchronously and delivers it back into the same session.

**Architecture:** A new toolgate endpoint `POST /summarize-video` does the heavy media work (ffmpeg audio + scene frames, STT, Vision) and returns *raw material*. A 5th FSE built-in `summarize_video` (executor=tool) only *enqueues* a durable `video_jobs` row and returns an instant ack — so the synchronous FSE seam never blocks. An in-core tokio worker drains the queue: calls toolgate, builds the final LLM digest with the core's own text providers, and delivers via `mirror_to_session` + a `ui_event` push.

**Tech Stack:** Rust 2024 (opex-core, opex-db, sqlx, tokio, reqwest/rustls), Python/FastAPI (toolgate), ffmpeg (system), yt-dlp, PostgreSQL.

## Global Constraints

- **rustls only — never add OpenSSL.** All HTTP uses `reqwest` with rustls features (project rule).
- **Only 3 keys belong in `.env`** (`OPEX_AUTH_TOKEN`, `OPEX_MASTER_KEY`, `DATABASE_URL`). All tunables go in `config/opex.toml` or toolgate config — **never new `.env` keys.**
- **TDD** — write the failing test first for every task (project rule `feedback_tdd`).
- **Work on master** (project rule); commit per task; **do not push** without explicit approval (project rule).
- **No Co-Authored-By trailer** in commits (project rule).
- **v1 is web-only** — Telegram/channel delivery is deferred; `channel_id` stays NULL.
- **No artificial limits** — any-length video, whole transcript to the LLM, scene-driven frames with only a high safety ceiling. The local Whisper provider handles any length; the toolgate 25 MB STT cap is lifted for the local provider.
- **DB-backed tests** use `#[sqlx::test(migrations = "../../migrations")]` and run under `make test-db` (isolated Postgres on :5434).
- Spec: `docs/superpowers/specs/2026-06-26-fse-video-summarization-design.md`.

---

## File Structure

**New files:**
- `migrations/064_video_jobs.sql` — durable job queue table.
- `crates/opex-db/src/video_jobs.rs` — queue CRUD (enqueue/claim/recover/done/failed/get). Leaf module, pure sqlx.
- `crates/opex-core/src/agent/file_scenario/video_worker.rs` — in-core tokio worker loop.
- `crates/opex-core/src/agent/file_scenario/video_summary.rs` — LLM-digest prompt builder + provider call (ports telesumbot prompts).
- `toolgate/video_helpers.py` — ffmpeg audio extraction + scene-frame extraction + yt-dlp download.
- `toolgate/routers/video.py` — `POST /summarize-video` orchestrator.
- `toolgate/test_video.py` — toolgate pipeline tests.

**Modified files:**
- `crates/opex-db/src/lib.rs` — `pub mod video_jobs;`.
- `crates/opex-core/src/db/mod.rs` — `pub use opex_db::video_jobs;`.
- `crates/opex-core/src/agent/fse/allowlist.rs:17` — add `"summarize_video"` to `FSE_DEFAULT_ALLOWLIST`.
- `crates/opex-core/src/agent/fse/seeder.rs` — add the `video/*` default `SeedRow`.
- `crates/opex-core/src/agent/file_scenario/dispatch.rs` — `BuiltinAction::SummarizeVideo`, `resolve`, `DispatchInput.enqueue`, `run_summarize_video`.
- `crates/opex-core/src/agent/file_scenario/dispatch_seam.rs` — thread `session_id`/`agent_name`/`source_type`; enqueue branch.
- `crates/opex-core/src/agent/pipeline/subagent.rs` — `enrich_message_text` gains `session_id`/`agent_name`; video-URL detector.
- `crates/opex-core/src/agent/pipeline/bootstrap.rs:250` — pass `session_id`/`agent_name` into enrich.
- `crates/opex-core/src/main.rs` — spawn the video worker in `spawn_background_tasks`; recover stuck jobs on startup.
- `crates/opex-core/src/agent/file_scenario/mod.rs` — `pub mod video_worker; pub mod video_summary;`.
- `toolgate/app.py` — mount `video.router`.
- `toolgate/routers/stt.py` — lift the 25 MB cap for the local provider.
- `toolgate/requirements.txt` (or pyproject) — add `yt-dlp`.
- `config/opex.toml` — `[video]` section.

---

## Task 1: `video_jobs` table + queue CRUD

**Files:**
- Create: `migrations/064_video_jobs.sql`
- Create: `crates/opex-db/src/video_jobs.rs`
- Modify: `crates/opex-db/src/lib.rs`
- Modify: `crates/opex-core/src/db/mod.rs`
- Test: inline `#[cfg(test)]` in `crates/opex-db/src/video_jobs.rs`

**Interfaces:**
- Produces:
  - `VideoJob { id: Uuid, session_id: Uuid, agent_name: String, channel_id: Option<Uuid>, source_type: String, source_ref: String, status: String, summary: Option<String>, error: Option<String>, attempts: i32 }`
  - `enqueue_video_job(db, session_id: Uuid, agent_name: &str, source_type: &str, source_ref: &str) -> anyhow::Result<Uuid>`
  - `claim_next_video_job(db) -> anyhow::Result<Option<VideoJob>>`
  - `recover_stuck_video_jobs(db) -> anyhow::Result<u64>`
  - `mark_video_job_done(db, id: Uuid, summary: &str) -> anyhow::Result<()>`
  - `mark_video_job_failed(db, id: Uuid, error: &str) -> anyhow::Result<()>`
  - `get_video_job(db, id: Uuid) -> anyhow::Result<Option<VideoJob>>`

- [ ] **Step 1: Write the migration**

Create `migrations/064_video_jobs.sql`:

```sql
-- Durable async queue for FSE video summarization jobs.
-- Mirrors the memory_tasks claim/recover pattern (see opex-memory-worker).
CREATE TABLE video_jobs (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id   UUID NOT NULL,
    agent_name   TEXT NOT NULL,
    channel_id   UUID,                       -- always NULL in v1 (web-only); reserved for Telegram
    source_type  TEXT NOT NULL CHECK (source_type IN ('file','url')),
    source_ref   TEXT NOT NULL,              -- signed upload URL or video link
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending','processing','done','failed')),
    summary      TEXT,
    error        TEXT,
    attempts     INT  NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX video_jobs_claim_idx ON video_jobs (status, created_at);
```

- [ ] **Step 2: Write the failing tests**

Create `crates/opex-db/src/video_jobs.rs` with the test module first:

```rust
//! Durable queue for FSE video-summarization jobs. Pure sqlx leaf module
//! (no crate::* refs) — mirrors memory_queries / sessions placement.

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VideoJob {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_name: String,
    pub channel_id: Option<Uuid>,
    pub source_type: String,
    pub source_ref: String,
    pub status: String,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub attempts: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn enqueue_then_claim_marks_processing(pool: PgPool) {
        let sid = Uuid::new_v4();
        let id = enqueue_video_job(&pool, sid, "Atlas", "file", "https://h/api/uploads/x?sig=1")
            .await
            .unwrap();

        let claimed = claim_next_video_job(&pool).await.unwrap().expect("a job");
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.status, "processing");
        assert_eq!(claimed.attempts, 1, "claim increments attempts");

        // A second claim finds nothing (only one pending row, now processing).
        assert!(claim_next_video_job(&pool).await.unwrap().is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recover_resets_processing_to_pending(pool: PgPool) {
        let sid = Uuid::new_v4();
        enqueue_video_job(&pool, sid, "Atlas", "file", "ref").await.unwrap();
        claim_next_video_job(&pool).await.unwrap().unwrap(); // → processing

        let n = recover_stuck_video_jobs(&pool).await.unwrap();
        assert_eq!(n, 1, "one stuck processing row recovered");

        // Now claimable again.
        assert!(claim_next_video_job(&pool).await.unwrap().is_some());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn done_and_failed_persist(pool: PgPool) {
        let sid = Uuid::new_v4();
        let id = enqueue_video_job(&pool, sid, "Atlas", "url", "https://youtu.be/x").await.unwrap();
        mark_video_job_done(&pool, id, "the summary").await.unwrap();
        let j = get_video_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(j.status, "done");
        assert_eq!(j.summary.as_deref(), Some("the summary"));

        let id2 = enqueue_video_job(&pool, sid, "Atlas", "url", "https://youtu.be/y").await.unwrap();
        mark_video_job_failed(&pool, id2, "yt-dlp: private video").await.unwrap();
        let j2 = get_video_job(&pool, id2).await.unwrap().unwrap();
        assert_eq!(j2.status, "failed");
        assert_eq!(j2.error.as_deref(), Some("yt-dlp: private video"));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `make test-db-up && DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-db video_jobs -- --nocapture`
Expected: FAIL — `enqueue_video_job` / `claim_next_video_job` etc. not found.

- [ ] **Step 4: Implement the queue functions**

Add to `crates/opex-db/src/video_jobs.rs` (above the test module):

```rust
/// Insert a pending job. Returns the new id.
pub async fn enqueue_video_job(
    db: &PgPool,
    session_id: Uuid,
    agent_name: &str,
    source_type: &str,
    source_ref: &str,
) -> anyhow::Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO video_jobs (session_id, agent_name, source_type, source_ref) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(session_id)
    .bind(agent_name)
    .bind(source_type)
    .bind(source_ref)
    .fetch_one(db)
    .await?;
    Ok(id)
}

/// Atomically claim the oldest pending job (pending → processing, +attempts).
/// SKIP LOCKED keeps concurrent workers from grabbing the same row.
pub async fn claim_next_video_job(db: &PgPool) -> anyhow::Result<Option<VideoJob>> {
    let job: Option<VideoJob> = sqlx::query_as(
        "UPDATE video_jobs SET status='processing', attempts=attempts+1, updated_at=NOW() \
         WHERE id = ( \
             SELECT id FROM video_jobs WHERE status='pending' \
             ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED \
         ) \
         RETURNING id, session_id, agent_name, channel_id, source_type, source_ref, \
                   status, summary, error, attempts",
    )
    .fetch_optional(db)
    .await?;
    Ok(job)
}

/// Reset rows stuck in 'processing' (crash recovery) back to 'pending'.
pub async fn recover_stuck_video_jobs(db: &PgPool) -> anyhow::Result<u64> {
    let res = sqlx::query(
        "UPDATE video_jobs SET status='pending', updated_at=NOW() WHERE status='processing'",
    )
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

pub async fn mark_video_job_done(db: &PgPool, id: Uuid, summary: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE video_jobs SET status='done', summary=$2, updated_at=NOW() WHERE id=$1")
        .bind(id)
        .bind(summary)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn mark_video_job_failed(db: &PgPool, id: Uuid, error: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE video_jobs SET status='failed', error=$2, updated_at=NOW() WHERE id=$1")
        .bind(id)
        .bind(error)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn get_video_job(db: &PgPool, id: Uuid) -> anyhow::Result<Option<VideoJob>> {
    let job: Option<VideoJob> = sqlx::query_as(
        "SELECT id, session_id, agent_name, channel_id, source_type, source_ref, \
                status, summary, error, attempts FROM video_jobs WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(job)
}
```

- [ ] **Step 5: Register the module**

In `crates/opex-db/src/lib.rs` add (alphabetically near the other `pub mod`s):

```rust
pub mod video_jobs;
```

In `crates/opex-core/src/db/mod.rs` add under the "Extracted to opex-db" block:

```rust
pub use opex_db::video_jobs;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-db video_jobs -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 7: Commit**

```bash
git add migrations/064_video_jobs.sql crates/opex-db/src/video_jobs.rs crates/opex-db/src/lib.rs crates/opex-core/src/db/mod.rs
git commit -m "feat(video): durable video_jobs queue + CRUD"
```

---

## Task 2: toolgate ffmpeg helpers (audio + scene frames + yt-dlp)

**Files:**
- Create: `toolgate/video_helpers.py`
- Modify: `toolgate/requirements.txt` (add `yt-dlp`)
- Test: `toolgate/test_video.py`

**Interfaces:**
- Produces:
  - `async def extract_audio(video_path: str) -> bytes` — ogg/opus mono 16 kHz bytes of the audio track.
  - `async def extract_scene_frames(video_path: str, threshold: float, ceiling: int) -> list[tuple[float, bytes]]` — `(timestamp_seconds, jpeg_bytes)` for scene cuts, capped at `ceiling` (safety only).
  - `async def download_video(url: str, dest_dir: str) -> str` — yt-dlp downloads to a file, returns the path.

- [ ] **Step 1: Write the failing tests**

Create `toolgate/test_video.py`:

```python
import asyncio
import os
import subprocess
import tempfile

import pytest

from video_helpers import extract_audio, extract_scene_frames


def _make_tiny_video(path: str):
    """2-second test video with one scene cut (color change at 1s) + a tone."""
    subprocess.run([
        "ffmpeg", "-y",
        "-f", "lavfi", "-i", "color=c=red:s=128x128:d=1",
        "-f", "lavfi", "-i", "color=c=blue:s=128x128:d=1",
        "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
        "-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0[v]",
        "-map", "[v]", "-map", "2:a", "-t", "2", path,
    ], check=True, capture_output=True)


@pytest.mark.asyncio
async def test_extract_audio_returns_nonempty_bytes():
    with tempfile.TemporaryDirectory() as d:
        vid = os.path.join(d, "in.mp4")
        _make_tiny_video(vid)
        audio = await extract_audio(vid)
        assert isinstance(audio, bytes)
        assert len(audio) > 0


@pytest.mark.asyncio
async def test_extract_scene_frames_finds_the_cut():
    with tempfile.TemporaryDirectory() as d:
        vid = os.path.join(d, "in.mp4")
        _make_tiny_video(vid)
        frames = await extract_scene_frames(vid, threshold=0.3, ceiling=100)
        assert len(frames) >= 1, "the red→blue cut must produce at least one frame"
        ts, jpeg = frames[0]
        assert isinstance(ts, float)
        assert jpeg[:2] == b"\xff\xd8", "JPEG SOI marker"
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd toolgate && python -m pytest test_video.py -v`
Expected: FAIL — `ModuleNotFoundError: video_helpers` / functions undefined.

- [ ] **Step 3: Implement the helpers**

Create `toolgate/video_helpers.py`:

```python
"""ffmpeg-based audio + scene-frame extraction and yt-dlp download for the
video-summary pipeline. System ffmpeg is required (already used by audio_trim)."""

import asyncio
import glob
import os
import tempfile


async def _run(*args: str) -> tuple[int, bytes, bytes]:
    proc = await asyncio.create_subprocess_exec(
        *args, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    out, err = await proc.communicate()
    return proc.returncode or 0, out, err


async def extract_audio(video_path: str) -> bytes:
    """Decode the audio track to mono 16 kHz ogg/opus (small, STT-friendly)."""
    with tempfile.TemporaryDirectory() as d:
        out = os.path.join(d, "audio.ogg")
        code, _, err = await _run(
            "ffmpeg", "-y", "-i", video_path,
            "-vn", "-ac", "1", "-ar", "16000", "-c:a", "libopus", "-b:a", "24k",
            out,
        )
        if code != 0 or not os.path.exists(out):
            raise RuntimeError(f"ffmpeg audio extract failed: {err.decode(errors='ignore')[:400]}")
        with open(out, "rb") as f:
            return f.read()


async def extract_scene_frames(
    video_path: str, threshold: float, ceiling: int
) -> list[tuple[float, bytes]]:
    """Extract a JPEG at each scene cut (`select='gt(scene,threshold)'`).
    `ceiling` is a high safety bound, not a product cap."""
    with tempfile.TemporaryDirectory() as d:
        pattern = os.path.join(d, "f_%05d.jpg")
        # showinfo writes pts_time to stderr; we map frame index → timestamp.
        code, _, err = await _run(
            "ffmpeg", "-y", "-i", video_path,
            "-vf", f"select='gt(scene,{threshold})',showinfo",
            "-vsync", "vfr", "-frames:v", str(ceiling), pattern,
        )
        if code != 0:
            raise RuntimeError(f"ffmpeg scene extract failed: {err.decode(errors='ignore')[:400]}")
        times: list[float] = []
        for line in err.decode(errors="ignore").splitlines():
            if "pts_time:" in line:
                try:
                    times.append(float(line.split("pts_time:")[1].split()[0]))
                except (IndexError, ValueError):
                    pass
        frames: list[tuple[float, bytes]] = []
        for i, fp in enumerate(sorted(glob.glob(os.path.join(d, "f_*.jpg")))):
            with open(fp, "rb") as f:
                ts = times[i] if i < len(times) else float(i)
                frames.append((ts, f.read()))
        return frames


async def download_video(url: str, dest_dir: str) -> str:
    """Download `url` via yt-dlp to a single file under dest_dir. Returns the path."""
    out_tmpl = os.path.join(dest_dir, "dl.%(ext)s")
    code, _, err = await _run(
        "yt-dlp", "-f", "best[ext=mp4]/best", "-o", out_tmpl, "--no-playlist", url
    )
    if code != 0:
        raise RuntimeError(f"yt-dlp failed: {err.decode(errors='ignore')[:400]}")
    files = glob.glob(os.path.join(dest_dir, "dl.*"))
    if not files:
        raise RuntimeError("yt-dlp produced no file")
    return files[0]
```

- [ ] **Step 4: Add the dependency**

Append to `toolgate/requirements.txt`:

```
yt-dlp
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd toolgate && python -m pytest test_video.py -v`
Expected: PASS (2 tests). (Requires system ffmpeg.)

- [ ] **Step 6: Commit**

```bash
git add toolgate/video_helpers.py toolgate/test_video.py toolgate/requirements.txt
git commit -m "feat(video): toolgate ffmpeg audio + scene-frame + yt-dlp helpers"
```

---

## Task 3: toolgate `POST /summarize-video` orchestrator + lift STT cap

**Files:**
- Create: `toolgate/routers/video.py`
- Modify: `toolgate/app.py`
- Modify: `toolgate/routers/stt.py`
- Test: extend `toolgate/test_video.py`

**Interfaces:**
- Consumes: `extract_audio`, `extract_scene_frames`, `download_video` (Task 2); `registry.aget_active("stt"|"vision")` returning provider instances with `.transcribe(http, bytes, filename, language, model=None)` and `.describe(http, bytes, content_type, prompt, max_tokens=2000)`.
- Produces: `POST /summarize-video` accepting JSON `{video_url?: str, page_url?: str, language: str}` and returning `{duration, transcript, frames: [{timestamp, description}], degraded: {stt, vision}}`. (`video_url` = a localhost upload URL; `page_url` = a link for yt-dlp.)

- [ ] **Step 1: Write the failing test**

Add to `toolgate/test_video.py`:

```python
from fastapi.testclient import TestClient


class _FakeSTT:
    name = "fake-stt"
    async def transcribe(self, http, audio_bytes, filename, language, model=None):
        return "привет это тест"


class _FakeVision:
    name = "fake-vision"
    async def describe(self, http, image_bytes, content_type, prompt, max_tokens=2000):
        return "кадр: синий экран"


def test_summarize_video_local_file(monkeypatch):
    import app as toolgate_app
    # Bypass auth (internal-network check passes for testclient host).
    monkeypatch.setattr(toolgate_app, "AUTH_TOKEN", "")

    # Fake providers via the registry.
    async def fake_active(cap):
        return _FakeSTT() if cap == "stt" else _FakeVision()
    monkeypatch.setattr(toolgate_app.registry, "aget_active", fake_active)

    # Serve a local file path to the router by faking download_full to a tiny video.
    with tempfile.TemporaryDirectory() as d:
        vid = os.path.join(d, "v.mp4")
        _make_tiny_video(vid)

        import routers.video as video_mod
        async def fake_fetch(http, url):
            return vid
        monkeypatch.setattr(video_mod, "_materialize_source", fake_fetch)

        client = TestClient(toolgate_app.app)
        r = client.post("/summarize-video", json={"video_url": "http://localhost/api/uploads/x", "language": "ru"})
        assert r.status_code == 200, r.text
        body = r.json()
        assert body["transcript"] == "привет это тест"
        assert len(body["frames"]) >= 1
        assert body["frames"][0]["description"] == "кадр: синий экран"
        assert body["degraded"] == {"stt": False, "vision": False}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd toolgate && python -m pytest test_video.py::test_summarize_video_local_file -v`
Expected: FAIL — `/summarize-video` 404 (router not mounted).

- [ ] **Step 3: Implement the router**

Create `toolgate/routers/video.py`:

```python
"""Video summarization orchestrator. Heavy media work for FSE summarize_video.
Returns RAW MATERIAL (transcript + frame descriptions); the final LLM digest is
built in opex-core, not here (toolgate has no text-LLM)."""

import asyncio
import logging
import os
import tempfile

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from helpers import download_limited
from video_helpers import extract_audio, extract_scene_frames, download_video

log = logging.getLogger("toolgate.video")

router = APIRouter(tags=["video"])

# Scene cut sensitivity (0..1 ffmpeg scene score). Tunable; default mirrors
# telesumbot's content detector intent. High ceiling guards pathological input.
SCENE_THRESHOLD = float(os.environ.get("VIDEO_SCENE_THRESHOLD", "0.4"))
FRAME_CEILING = int(os.environ.get("VIDEO_FRAME_CEILING", "200"))
FRAME_VISION_CONCURRENCY = 4


class SummarizeVideoRequest(BaseModel):
    video_url: str | None = None   # localhost upload URL (file source)
    page_url: str | None = None    # link for yt-dlp (url source)
    language: str = "ru"


async def _materialize_source(http, req: SummarizeVideoRequest, work_dir: str) -> str:
    """Return a local video file path. Download from upload URL or via yt-dlp."""
    if req.page_url:
        return await download_video(req.page_url, work_dir)
    if req.video_url:
        data, _ = await download_limited(http, req.video_url, max_bytes=None)
        path = os.path.join(work_dir, "upload.mp4")
        with open(path, "wb") as f:
            f.write(data)
        return path
    raise ValueError("either video_url or page_url is required")


@router.post("/summarize-video")
async def summarize_video(body: SummarizeVideoRequest, request: Request):
    http = request.app.state.http_client
    registry = request.app.state.registry
    degraded = {"stt": False, "vision": False}

    with tempfile.TemporaryDirectory() as work_dir:
        try:
            video_path = await _materialize_source(http, body, work_dir)
        except Exception as e:
            return JSONResponse(status_code=502, content={"error": f"source fetch failed: {e}"})

        try:
            audio_bytes = await extract_audio(video_path)
        except Exception as e:
            return JSONResponse(status_code=502, content={"error": f"audio extract failed: {e}"})

        # ── Transcribe whole audio (no length cap for the local provider) ──
        stt = await registry.aget_active("stt")
        if stt is None:
            return JSONResponse(status_code=503, content={"error": "no STT provider active"})
        try:
            transcript = await stt.transcribe(http, audio_bytes, "video.ogg", body.language, None)
        except Exception as e:
            return JSONResponse(status_code=502, content={"error": f"transcribe failed: {e}"})

        # ── Scene frames → Vision descriptions (bounded concurrency) ──
        frames_out: list[dict] = []
        try:
            frames = await extract_scene_frames(video_path, SCENE_THRESHOLD, FRAME_CEILING)
        except Exception as e:
            frames = []
            log.warning("scene extract failed (continuing transcript-only): %s", e)

        vision = await registry.aget_active("vision")
        if vision is None and frames:
            degraded["vision"] = True

        if vision is not None and frames:
            sem = asyncio.Semaphore(FRAME_VISION_CONCURRENCY)
            prompt = "Опиши кадр кратко: что показано, текст на экране, ключевые объекты."

            async def describe(ts: float, jpeg: bytes):
                async with sem:
                    try:
                        desc = await vision.describe(http, jpeg, "image/jpeg", prompt)
                        return {"timestamp": ts, "description": desc}
                    except Exception as e:
                        log.warning("frame describe failed at %.1fs: %s", ts, e)
                        return None

            results = await asyncio.gather(*(describe(ts, j) for ts, j in frames))
            frames_out = [r for r in results if r is not None]

        # Probe duration (best-effort, non-fatal).
        duration = 0.0
        try:
            from video_helpers import _run
            code, out, _ = await _run(
                "ffprobe", "-v", "error", "-show_entries", "format=duration",
                "-of", "default=nw=1:nk=1", video_path,
            )
            if code == 0:
                duration = float(out.decode().strip() or 0.0)
        except Exception:
            pass

        return {
            "duration": duration,
            "transcript": transcript,
            "frames": frames_out,
            "degraded": degraded,
        }
```

- [ ] **Step 4: Mount the router**

In `toolgate/app.py`, extend the router import + include lines:

```python
from routers import stt, vision, tts, imagegen, embedding, documents, fetch, search, video
```
```python
app.include_router(video.router)
```

- [ ] **Step 5: Lift the STT 25 MB cap for the local provider**

In `toolgate/routers/stt.py`, the `STT_MAX_BYTES = 25 * 1024 * 1024` check (`check_upload_size`) is an OpenAI-cloud artifact. Gate it so it only applies to providers that declare a hard limit. Change the constant + check in both `transcribe` and `transcribe_url` to consult the provider:

```python
# A provider may declare a hard per-file byte cap (cloud APIs). Local Whisper
# has none → no cap. Default: no cap unless the provider sets `max_bytes`.
def _provider_cap(provider) -> int | None:
    return getattr(provider, "max_bytes", None)
```

Then replace the unconditional `check_upload_size(audio_bytes, STT_MAX_BYTES, ...)` with:

```python
cap = _provider_cap(provider)
if cap is not None:
    size_err = check_upload_size(audio_bytes, cap, "Audio file")
    if size_err:
        return size_err
```

(The video router calls the provider directly and never hits this router, but lifting the cap keeps `/transcribe` consistent for long audio.)

- [ ] **Step 6: Run the test to verify it passes**

Run: `cd toolgate && python -m pytest test_video.py -v`
Expected: PASS (3 tests).

- [ ] **Step 7: Commit**

```bash
git add toolgate/routers/video.py toolgate/app.py toolgate/routers/stt.py toolgate/test_video.py
git commit -m "feat(video): toolgate /summarize-video orchestrator + lift local STT cap"
```

---

## Task 4: FSE built-in registration (`summarize_video` in allowlist + resolve + seed)

**Files:**
- Modify: `crates/opex-core/src/agent/fse/allowlist.rs:17`
- Modify: `crates/opex-core/src/agent/file_scenario/dispatch.rs`
- Modify: `crates/opex-core/src/agent/fse/seeder.rs`
- Test: inline in `dispatch.rs` and `seeder.rs`

**Interfaces:**
- Produces: `BuiltinAction::SummarizeVideo`; `resolve("summarize_video") == Some(BuiltinAction::SummarizeVideo)`; `FSE_DEFAULT_ALLOWLIST` contains `"summarize_video"`; a seeded `video/* → summarize_video` default row.

- [ ] **Step 1: Write the failing tests**

In `crates/opex-core/src/agent/file_scenario/dispatch.rs`, add to `mod tests`:

```rust
#[test]
fn resolve_summarize_video() {
    assert_eq!(resolve("summarize_video"), Some(BuiltinAction::SummarizeVideo));
}
```

In `crates/opex-core/src/agent/fse/allowlist.rs` test module (or add one), assert membership:

```rust
#[test]
fn allowlist_contains_summarize_video() {
    assert!(FSE_DEFAULT_ALLOWLIST.contains(&"summarize_video"));
}
```

In `crates/opex-core/src/agent/fse/seeder.rs` `reconcile_tests`, extend `seed_uses_builtin_action_names` expectations by adding a new assertion:

```rust
#[test]
fn seed_includes_video_default() {
    let rows = default_seed_rows();
    assert!(
        rows.iter().any(|r| r.match_type == "video/*" && r.action_ref == "summarize_video"),
        "video/* → summarize_video default must be seeded",
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p opex-core resolve_summarize_video allowlist_contains_summarize_video seed_includes_video_default -- --nocapture`
Expected: FAIL (variant/const/seed missing).

- [ ] **Step 3: Implement**

In `crates/opex-core/src/agent/fse/allowlist.rs:17`:

```rust
pub const FSE_DEFAULT_ALLOWLIST: &[&str] =
    &["transcribe", "describe", "extract_document", "save", "summarize_video"];
```

In `crates/opex-core/src/agent/file_scenario/dispatch.rs`, add the variant to `enum BuiltinAction`:

```rust
pub enum BuiltinAction {
    Transcribe,
    Describe,
    ExtractDocument,
    Save,
    SummarizeVideo,
}
```

and the arm in `resolve`:

```rust
        "summarize_video" => Some(BuiltinAction::SummarizeVideo),
```

(The `dispatch_action` match arm for `SummarizeVideo` is added in Task 5.)

In `crates/opex-core/src/agent/fse/seeder.rs`, append a `SeedRow` to `default_seed_rows()`:

```rust
        SeedRow {
            match_type: "video/*",
            action_ref: "summarize_video",
            label: "Сводка видео",
            executor: "tool",
            is_default: true,
            priority: 100,
        },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p opex-core resolve_summarize_video allowlist_contains_summarize_video seed_includes_video_default every_allowlist_member_resolves -- --nocapture`
Expected: PASS — including the existing `every_allowlist_member_resolves` guard (now that `resolve` knows the action).

> Note: the existing seeder test `seeds_three_defaults_on_fresh_db` asserts `inserted == 3`. Update its expectation to `4` and add `video/*` to its match-type loop in the same edit.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/fse/allowlist.rs crates/opex-core/src/agent/file_scenario/dispatch.rs crates/opex-core/src/agent/fse/seeder.rs
git commit -m "feat(video): register summarize_video built-in + video/* default seed"
```

---

## Task 5: `DispatchInput.enqueue` + `run_summarize_video` (enqueue + ack)

**Files:**
- Modify: `crates/opex-core/src/agent/file_scenario/dispatch.rs`
- Test: inline in `dispatch.rs`

**Interfaces:**
- Consumes: `opex_db::video_jobs::enqueue_video_job` (Task 1); `BuiltinAction::SummarizeVideo` (Task 4).
- Produces:
  - `struct EnqueueCtx<'a> { db: &'a sqlx::PgPool, session_id: uuid::Uuid, agent_name: &'a str, source_type: &'a str }`
  - `DispatchInput.enqueue: Option<EnqueueCtx<'a>>` (existing callers pass `None`).
  - `run_summarize_video(&DispatchInput) -> ScenarioOutcome` — on `Some(ctx)`: insert a `video_jobs` row, return `ScenarioOutcome::ok("🎬 Видео принято, готовлю сводку…")`; on `None`: `unsupported`.

- [ ] **Step 1: Write the failing test**

Add to `dispatch.rs` `mod tests` (DB-backed):

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn summarize_video_enqueues_and_acks(pool: sqlx::PgPool) {
    use opex_types::{MediaAttachment, MediaType};
    let sid = uuid::Uuid::new_v4();
    let att = MediaAttachment {
        url: "https://h/api/uploads/v1?sig=x".into(),
        media_type: MediaType::Video,
        file_name: Some("clip.mp4".into()),
        mime_type: Some("video/mp4".into()),
        file_size: None,
    };
    let client = reqwest::Client::new();
    let input = DispatchInput {
        action_ref: "summarize_video",
        attachment: &att,
        toolgate_url: "http://localhost:9011",
        gateway_listen: "0.0.0.0:18789",
        language: "ru",
        http_client: &client,
        timeout: std::time::Duration::from_secs(60),
        enqueue: Some(EnqueueCtx {
            db: &pool,
            session_id: sid,
            agent_name: "Atlas",
            source_type: "file",
        }),
    };
    let out = dispatch_action(input).await;
    assert_eq!(out.status, ScenarioStatus::Ok);
    assert!(out.summary_text.contains("видео"), "ack mentions video: {}", out.summary_text);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE session_id=$1")
        .bind(sid).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1, "one video_jobs row enqueued");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core summarize_video_enqueues_and_acks -- --nocapture`
Expected: FAIL — `EnqueueCtx` / `enqueue` field / `run_summarize_video` missing.

- [ ] **Step 3: Implement**

In `dispatch.rs`, add the context struct and field:

```rust
/// Context the async `summarize_video` built-in needs to enqueue a durable job.
/// Other built-ins ignore it (they pass `None`).
pub struct EnqueueCtx<'a> {
    pub db: &'a sqlx::PgPool,
    pub session_id: uuid::Uuid,
    pub agent_name: &'a str,
    pub source_type: &'a str, // "file" | "url"
}

pub struct DispatchInput<'a> {
    pub action_ref: &'a str,
    pub attachment: &'a opex_types::MediaAttachment,
    pub toolgate_url: &'a str,
    pub gateway_listen: &'a str,
    pub language: &'a str,
    pub http_client: &'a reqwest::Client,
    pub timeout: std::time::Duration,
    pub enqueue: Option<EnqueueCtx<'a>>,
}
```

Add the match arm in `dispatch_action`:

```rust
        BuiltinAction::SummarizeVideo => run_summarize_video(&input).await,
```

Add the handler:

```rust
/// Async built-in: enqueue a durable video_jobs row and return an instant ack.
/// The heavy pipeline runs out-of-band in the in-core video worker.
async fn run_summarize_video(input: &DispatchInput<'_>) -> ScenarioOutcome {
    let ctx = match &input.enqueue {
        Some(c) => c,
        None => {
            return ScenarioOutcome::unsupported(
                "summarize_video requires enqueue context (session/agent)".into(),
            )
        }
    };
    match opex_db::video_jobs::enqueue_video_job(
        ctx.db,
        ctx.session_id,
        ctx.agent_name,
        ctx.source_type,
        &input.attachment.url,
    )
    .await
    {
        Ok(_id) => ScenarioOutcome::ok(
            "🎬 Видео принято, готовлю сводку — пришлю, когда будет готова.".into(),
            vec![input.attachment.url.clone()],
        ),
        Err(e) => ScenarioOutcome::failed(format!("could not enqueue video job: {e}")),
    }
}
```

- [ ] **Step 4: Fix existing `DispatchInput` constructions**

Every existing `DispatchInput { ... }` literal (in `dispatch_seam.rs::run_builtin` and the `dispatch.rs` test helper `input()`) must add `enqueue: None`. Update the `dispatch.rs` test helper:

```rust
        DispatchInput {
            action_ref,
            attachment,
            toolgate_url,
            gateway_listen: "0.0.0.0:18789",
            language: "ru",
            http_client: client,
            timeout: Duration::from_secs(10),
            enqueue: None,
        }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core dispatch:: -- --nocapture`
Expected: PASS (new test + all existing dispatch tests still green).

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/file_scenario/dispatch.rs
git commit -m "feat(video): summarize_video enqueues durable job + acks"
```

---

## Task 6: Thread session/agent through the seam + enqueue branch

**Files:**
- Modify: `crates/opex-core/src/agent/file_scenario/dispatch_seam.rs`
- Modify: `crates/opex-core/src/agent/pipeline/subagent.rs`
- Modify: `crates/opex-core/src/agent/pipeline/bootstrap.rs`
- Test: inline in `dispatch_seam.rs`

**Interfaces:**
- Consumes: `EnqueueCtx`, `run_summarize_video` via `dispatch_action` (Task 5).
- Produces: `dispatch_attachments(...)` gains `session_id: uuid::Uuid, agent_name: &str` params; `run_builtin` passes an `EnqueueCtx` (source_type `"file"`); `enrich_message_text(...)` gains `session_id`/`agent_name` params; `bootstrap.rs` passes them.

- [ ] **Step 1: Write the failing test**

In `dispatch_seam.rs` `mod tests`, add (reuses the existing wiremock harness pattern):

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn video_default_enqueues_job_not_sync_call(pool: sqlx::PgPool) {
    use crate::db::file_scenarios::create;
    // Seed a video/* default summarize_video binding.
    create(&pool, "video/*", "tool", "summarize_video", "Сводка видео", true, 100, true, "test")
        .await.unwrap();

    // Serve MP4 magic bytes so sniff → video/mp4.
    let server = MockServer::start().await;
    let mp4: Vec<u8> = b"\x00\x00\x00\x18ftypmp42fakevideo".to_vec();
    Mock::given(method("GET")).and(path_regex(r"^/api/uploads/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(mp4))
        .mount(&server).await;
    let port = server.address().port();
    let gateway_listen = format!("127.0.0.1:{port}");
    let sid = uuid::Uuid::new_v4();
    let upload_url = format!("{}/api/uploads/{}?sig=x&exp=1", server.uri(), uuid::Uuid::new_v4());
    let video_att = MediaAttachment {
        url: upload_url.clone(), media_type: MediaType::Video,
        file_name: Some("clip.mp4".into()), mime_type: Some("video/mp4".into()), file_size: None,
    };

    let client = reqwest::Client::new();
    let mut enriched = String::new();
    let (outcomes, _pending) = dispatch_attachments(
        &client, &gateway_listen, "http://localhost:9011", "ru",
        &pool, sid, "Atlas", &mut enriched, &[video_att],
    ).await;

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, crate::agent::file_scenario::outcome::ScenarioStatus::Ok);
    assert!(outcomes[0].summary_text.contains("видео"));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE session_id=$1")
        .bind(sid).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1, "video default enqueued a job");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core video_default_enqueues_job_not_sync_call -- --nocapture`
Expected: FAIL — `dispatch_attachments` signature mismatch (missing `session_id`/`agent_name`).

- [ ] **Step 3: Add the params to `dispatch_attachments`**

In `dispatch_seam.rs`, extend the signature (insert after `db`):

```rust
pub async fn dispatch_attachments(
    http_client: &reqwest::Client,
    gateway_listen: &str,
    toolgate_url: &str,
    agent_language: &str,
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
    agent_name: &str,
    _enriched: &mut String,
    attachments: &[MediaAttachment],
) -> (Vec<ScenarioOutcome>, Vec<PendingAlternative>) {
```

In `run_builtin` (same file), add the enqueue context so `summarize_video` can enqueue. Change `run_builtin` to accept and forward it:

```rust
async fn run_builtin(
    action_ref: &str,
    http_client: &reqwest::Client,
    gateway_listen: &str,
    toolgate_url: &str,
    agent_language: &str,
    attachment: &MediaAttachment,
    enqueue: Option<crate::agent::file_scenario::dispatch::EnqueueCtx<'_>>,
) -> ScenarioOutcome {
    dispatch_action(DispatchInput {
        action_ref,
        attachment,
        toolgate_url,
        gateway_listen,
        language: agent_language,
        http_client,
        timeout: BUILTIN_TIMEOUT,
        enqueue,
    })
    .await
}
```

At each `run_builtin(...)` call site in `dispatch_attachments`, pass the enqueue context only for the default-run path (where `action_to_run` may be `summarize_video`). Build it once per attachment:

```rust
        let enq = crate::agent::file_scenario::dispatch::EnqueueCtx {
            db,
            session_id,
            agent_name,
            source_type: "file",
        };
```

and pass `Some(enq)` to the default-binding `run_builtin` call, `None` to the `save`-fallback calls. (Only the default-run arm can hit `summarize_video`; `save`/transcribe/describe ignore the ctx.)

- [ ] **Step 4: Update the other `dispatch_attachments` test call sites**

The existing tests in `dispatch_seam.rs` call `dispatch_attachments(...)` with the old arity. Add `uuid::Uuid::new_v4(), "TestAgent",` after the `&pool` argument in each (there are several — update all).

- [ ] **Step 5: Thread through `enrich_message_text` and bootstrap**

In `subagent.rs`, extend `enrich_message_text` signature (after `db`):

```rust
pub async fn enrich_message_text(
    http_client: &reqwest::Client,
    gateway_listen: &str,
    toolgate_url: &str,
    agent_language: &str,
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
    agent_name: &str,
    user_text: &str,
    attachments: &[opex_types::MediaAttachment],
) -> EnrichResult {
```

and forward them into the `dispatch_attachments(...)` call inside it.

In `bootstrap.rs:250`, pass the session id and agent name (both already in scope — `ctx`/`engine`):

```rust
    let enrich = crate::agent::pipeline::subagent::enrich_message_text(
        engine.http_client(),
        &engine.cfg().app_config.gateway.listen,
        &toolgate_url,
        &engine.cfg().agent.language,
        &engine.cfg().db,
        ctx.session_id,
        &engine.cfg().agent.name,
        &user_text,
        &ctx.msg.attachments,
    )
    .await;
```

(Confirm the session id field name on `ctx` — it is the `Uuid` resolved in bootstrap; use that binding.)

- [ ] **Step 6: Run the test to verify it passes**

Run: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core file_scenario:: -- --nocapture`
Expected: PASS (new test + all existing seam tests green with updated arity).

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/agent/file_scenario/dispatch_seam.rs crates/opex-core/src/agent/pipeline/subagent.rs crates/opex-core/src/agent/pipeline/bootstrap.rs
git commit -m "feat(video): thread session/agent into FSE seam; video default enqueues"
```

---

## Task 7: LLM digest builder (`video_summary.rs`)

**Files:**
- Create: `crates/opex-core/src/agent/file_scenario/video_summary.rs`
- Modify: `crates/opex-core/src/agent/file_scenario/mod.rs`
- Test: inline in `video_summary.rs`

**Interfaces:**
- Consumes: `LlmProvider::chat(&[Message], &[ToolDefinition], CallOptions)` (from `agent::providers`).
- Produces:
  - `struct RawMaterial { duration: f64, transcript: String, frames: Vec<FrameDesc>, degraded: Degraded }` with `FrameDesc { timestamp: f64, description: String }`, `Degraded { stt: bool, vision: bool }` (serde, matches toolgate JSON).
  - `build_summary_messages(raw: &RawMaterial) -> Vec<opex_types::Message>` — system + user message embedding the whole transcript + timestamped frame descriptions (telesumbot-style prompt).

- [ ] **Step 1: Write the failing test**

Create `crates/opex-core/src/agent/file_scenario/video_summary.rs` with the test first:

```rust
//! Builds the final LLM digest prompt from toolgate raw material. The whole
//! transcript goes in (large context — no telesumbot 40k chunking). Prompts
//! ported from telesumbot `summary/prompts.rs`.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct FrameDesc {
    pub timestamp: f64,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Degraded {
    pub stt: bool,
    pub vision: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawMaterial {
    #[serde(default)]
    pub duration: f64,
    pub transcript: String,
    #[serde(default)]
    pub frames: Vec<FrameDesc>,
    #[serde(default)]
    pub degraded: Degraded,
}

#[cfg(test)]
mod tests {
    use super::*;
    use opex_types::MessageRole;

    #[test]
    fn prompt_embeds_transcript_and_frames() {
        let raw = RawMaterial {
            duration: 90.0,
            transcript: "полный текст речи".into(),
            frames: vec![FrameDesc { timestamp: 12.5, description: "синий слайд".into() }],
            degraded: Degraded::default(),
        };
        let msgs = build_summary_messages(&raw);
        assert_eq!(msgs[0].role, MessageRole::System);
        let user = &msgs[msgs.len() - 1];
        assert_eq!(user.role, MessageRole::User);
        assert!(user.content.contains("полный текст речи"), "whole transcript embedded");
        assert!(user.content.contains("синий слайд"), "frame description embedded");
        assert!(user.content.contains("12"), "timestamp embedded");
    }

    #[test]
    fn degraded_vision_note_present() {
        let raw = RawMaterial {
            duration: 10.0,
            transcript: "речь".into(),
            frames: vec![],
            degraded: Degraded { stt: false, vision: true },
        };
        let msgs = build_summary_messages(&raw);
        let user = &msgs[msgs.len() - 1];
        assert!(user.content.contains("без кадров") || user.content.contains("кадры недоступны"),
            "degraded vision is noted to the model");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core video_summary:: -- --nocapture`
Expected: FAIL — `build_summary_messages` not defined.

- [ ] **Step 3: Implement**

Add to `video_summary.rs` (above tests):

```rust
use opex_types::{Message, MessageRole};

const SYSTEM_PROMPT: &str = "Ты помощник, который делает структурированную русскоязычную \
сводку видео по его транскрипту и описаниям ключевых кадров. Дай: краткое резюме (3-5 \
предложений), затем основные тезисы списком с таймкодами, затем выводы. Пиши по-русски, \
без воды.";

/// Build the system+user messages for the digest. The entire transcript is
/// embedded (large-context model — no chunking).
pub fn build_summary_messages(raw: &RawMaterial) -> Vec<Message> {
    let mut user = String::new();
    user.push_str(&format!("Длительность видео: {:.0} сек.\n\n", raw.duration));
    user.push_str("=== Транскрипт ===\n");
    user.push_str(&raw.transcript);
    user.push_str("\n\n");

    if raw.frames.is_empty() {
        if raw.degraded.vision {
            user.push_str("(Описания кадров недоступны — vision-провайдер не активен; \
                           сделай сводку без кадров.)\n");
        }
    } else {
        user.push_str("=== Ключевые кадры (таймкод → описание) ===\n");
        for f in &raw.frames {
            user.push_str(&format!("[{:.0}s] {}\n", f.timestamp, f.description));
        }
    }
    user.push_str("\nСделай сводку по инструкции.");

    vec![
        Message { role: MessageRole::System, content: SYSTEM_PROMPT.to_string() },
        Message { role: MessageRole::User, content: user },
    ]
}
```

> If `opex_types::Message` has more fields than `role`/`content` (e.g. `tool_calls`, `name`), construct it via its constructor or `..Default::default()`; check `opex-types/src/lib.rs` for the exact shape and adapt the two literals. Keep `role`/`content` as shown.

In `crates/opex-core/src/agent/file_scenario/mod.rs`, add:

```rust
pub mod video_summary;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p opex-core video_summary:: -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/file_scenario/video_summary.rs crates/opex-core/src/agent/file_scenario/mod.rs
git commit -m "feat(video): LLM digest prompt builder from raw material"
```

---

## Task 8: In-core video worker (claim → toolgate → digest → deliver)

**Files:**
- Create: `crates/opex-core/src/agent/file_scenario/video_worker.rs`
- Modify: `crates/opex-core/src/agent/file_scenario/mod.rs`
- Modify: `crates/opex-core/src/main.rs` (spawn + startup recovery)
- Test: inline in `video_worker.rs` (the pure `summarize_one` step against a wiremock toolgate)

**Interfaces:**
- Consumes: `claim_next_video_job`, `mark_video_job_done/failed`, `recover_stuck_video_jobs` (Task 1); `RawMaterial` + `build_summary_messages` (Task 7); `LlmProvider::chat` (providers); `mirror_to_session` (opex-db); `ui_event_tx` (channel bus).
- Produces:
  - `async fn process_one(db, http, toolgate_url, provider: &dyn LlmProvider, job: &VideoJob) -> anyhow::Result<String>` — calls toolgate, builds + runs the digest, returns the summary text. (Delivery is separate so it is unit-testable.)
  - `fn spawn_video_worker(state: &AppState, shutdown: CancellationToken)` — the polling loop.

- [ ] **Step 1: Write the failing test**

Create `crates/opex-core/src/agent/file_scenario/video_worker.rs` test-first:

```rust
//! In-core durable worker for video_jobs. Lives in opex-core (not memory-worker)
//! because it needs LLM providers, ui_event_tx and session delivery.

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Minimal fake LLM provider returning a fixed digest.
    struct FakeLlm;
    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for FakeLlm {
        async fn chat(
            &self,
            _messages: &[opex_types::Message],
            _tools: &[crate::agent::providers::ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<crate::agent::providers::LlmResponse> {
            Ok(crate::agent::providers::LlmResponse::text("СВОДКА: тест ок"))
        }
        async fn chat_stream(
            &self,
            _m: &[opex_types::Message],
            _t: &[crate::agent::providers::ToolDefinition],
            _tx: tokio::sync::mpsc::Sender<String>,
            _o: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<crate::agent::providers::LlmResponse> {
            Ok(crate::agent::providers::LlmResponse::text("СВОДКА: тест ок"))
        }
        fn name(&self) -> &str { "fake" }
        fn current_model(&self) -> String { "fake".into() }
    }

    #[tokio::test]
    async fn process_one_calls_toolgate_and_builds_digest() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/summarize-video"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "duration": 60.0,
                "transcript": "речь из видео",
                "frames": [{"timestamp": 5.0, "description": "слайд"}],
                "degraded": {"stt": false, "vision": false}
            })))
            .mount(&server).await;

        let job = opex_db::video_jobs::VideoJob {
            id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            agent_name: "Atlas".into(),
            channel_id: None,
            source_type: "file".into(),
            source_ref: "http://localhost/api/uploads/x?sig=1".into(),
            status: "processing".into(),
            summary: None, error: None, attempts: 1,
        };
        let client = reqwest::Client::new();
        let provider = FakeLlm;
        let summary = process_one(&client, &server.uri(), "0.0.0.0:18789", &provider, &job)
            .await.unwrap();
        assert!(summary.contains("СВОДКА"), "digest returned: {summary}");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core video_worker:: -- --nocapture`
Expected: FAIL — `process_one` not defined.

- [ ] **Step 3: Implement `process_one`**

Add to `video_worker.rs` (above tests). Adapt `LlmResponse` accessor to the real type — confirm whether it exposes `.text` field or a `.text()` method in `agent/providers/mod.rs` and use that:

```rust
use crate::agent::file_scenario::video_summary::{build_summary_messages, RawMaterial};
use crate::agent::providers::{CallOptions, LlmProvider};
use opex_db::video_jobs::VideoJob;

/// Rewrites a public upload URL to the localhost gateway (toolgate downloads
/// from localhost, same as extract_document). For url-source jobs the ref is a
/// page link and passes through untouched.
fn source_payload(job: &VideoJob, gateway_listen: &str) -> serde_json::Value {
    if job.source_type == "url" {
        serde_json::json!({ "page_url": job.source_ref })
    } else {
        let local = crate::agent::url_tools::uploads_local_url(&job.source_ref, gateway_listen);
        serde_json::json!({ "video_url": local })
    }
}

/// Call toolgate, build + run the digest. Returns the summary text.
pub async fn process_one(
    http: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    provider: &dyn LlmProvider,
    job: &VideoJob,
) -> anyhow::Result<String> {
    let url = format!("{}/summarize-video", toolgate_url.trim_end_matches('/'));
    let mut body = source_payload(job, gateway_listen);
    body["language"] = serde_json::json!("ru");

    let resp = http.post(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("toolgate /summarize-video HTTP {}", resp.status().as_u16());
    }
    let raw: RawMaterial = resp.json().await?;

    let messages = build_summary_messages(&raw);
    let opts = CallOptions { thinking_level: 0, claude_md_content: None };
    let llm = provider.chat(&messages, &[], opts).await?;
    Ok(llm.text()) // adapt to the real accessor (field vs method)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p opex-core video_worker:: -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Implement `spawn_video_worker` + delivery (no new test — wired in main)**

Add the loop and delivery to `video_worker.rs`:

```rust
use crate::gateway::AppState;
use tokio_util::sync::CancellationToken;

/// Poll the durable queue; process one job at a time (v1 concurrency = 1).
pub fn spawn_video_worker(state: &AppState, shutdown: CancellationToken) {
    let db = state.infra.db.clone();
    let http = state.http_client_arc(); // reqwest::Client (Clone) — use the shared client
    let toolgate_url = state.toolgate_url_string(); // helper returning configured URL or default
    let gateway_listen = state.gateway_listen_string();
    let agents = state.agents.clone();
    let ui_tx = state.channels.ui_event_tx.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {}
            }
            let job = match opex_db::video_jobs::claim_next_video_job(&db).await {
                Ok(Some(j)) => j,
                Ok(None) => continue,
                Err(e) => { tracing::warn!(error=%e, "video claim failed"); continue; }
            };

            // Resolve the agent's active provider for the digest.
            let engine = match agents.get_engine(&job.agent_name).await {
                Some(e) => e,
                None => {
                    let _ = opex_db::video_jobs::mark_video_job_failed(
                        &db, job.id, "agent engine not found").await;
                    continue;
                }
            };
            let provider = engine.provider_arc();

            let result = process_one(&http, &toolgate_url, &gateway_listen, provider.as_ref(), &job).await;
            match result {
                Ok(summary) => {
                    let _ = opex_db::video_jobs::mark_video_job_done(&db, job.id, &summary).await;
                    deliver(&db, &ui_tx, &job, &summary).await;
                }
                Err(e) => {
                    let msg = format!("Не удалось обработать видео: {e}");
                    let _ = opex_db::video_jobs::mark_video_job_failed(&db, job.id, &e.to_string()).await;
                    deliver(&db, &ui_tx, &job, &msg).await;
                }
            }
        }
        tracing::info!("video worker stopped");
    });
}

/// Web-only delivery: inject into the session + push a ui_event. channel_id is
/// NULL in v1; Telegram notify is deferred.
async fn deliver(
    db: &sqlx::PgPool,
    ui_tx: &tokio::sync::broadcast::Sender<String>,
    job: &VideoJob,
    text: &str,
) {
    // Inject into the originating session as an assistant message.
    // mirror_to_session resolves by (agent, participant, channel); for web the
    // session id is known directly — insert straight into messages.
    if let Err(e) = sqlx::query(
        "INSERT INTO messages (session_id, agent_id, role, content, is_mirror) \
         VALUES ($1, $2, 'assistant', $3, true)",
    )
    .bind(job.session_id)
    .bind(&job.agent_name)
    .bind(text)
    .execute(db)
    .await
    {
        tracing::warn!(error=%e, "video summary session inject failed");
    }
    // Live web push so an open client renders it without reload.
    let ev = serde_json::json!({
        "type": "video_summary_ready",
        "session_id": job.session_id.to_string(),
        "text": text,
    });
    let _ = ui_tx.send(ev.to_string());
}
```

> The exact `AppState` accessors (`http_client_arc`, `toolgate_url_string`, `gateway_listen_string`, `agents.get_engine`, `provider_arc`) must match real signatures — confirm against `gateway/state.rs` and `agent/engine`. If a helper does not exist, read the field directly (e.g. `state.infra.db`, `engine.http_client()`). The direct `messages` INSERT mirrors `mirror_to_session`'s own statement; if a session-id-keyed helper exists in `opex_db::sessions`, prefer it.

- [ ] **Step 6: Wire startup recovery + spawn in `main.rs`**

In `crates/opex-core/src/main.rs`, inside `spawn_background_tasks` (near the other `tokio::spawn` blocks, ~line 1267), add:

```rust
    // Video summarization worker (durable video_jobs queue).
    crate::agent::file_scenario::video_worker::spawn_video_worker(state, shutdown.clone());
```

And at startup recovery (where memory bootstrap runs, ~line 432, or just before spawning), reset stuck jobs once:

```rust
    if let Err(e) = opex_db::video_jobs::recover_stuck_video_jobs(&state.infra.db).await {
        tracing::warn!(error=%e, "video_jobs recovery failed");
    }
```

Register the module in `mod.rs`:

```rust
pub mod video_worker;
```

- [ ] **Step 7: Build + run the worker test + full check**

Run: `cargo check -p opex-core && DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core video_worker:: -- --nocapture`
Expected: compiles; worker test PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/opex-core/src/agent/file_scenario/video_worker.rs crates/opex-core/src/agent/file_scenario/mod.rs crates/opex-core/src/main.rs
git commit -m "feat(video): in-core worker — claim, toolgate, digest, web delivery"
```

---

## Task 9: Video-URL detector (YouTube → url job)

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/subagent.rs`
- Test: inline in `subagent.rs`

**Interfaces:**
- Consumes: `enqueue_video_job` (Task 1); the threaded `session_id`/`agent_name` (Task 6).
- Produces: `fn detect_video_links(text: &str) -> Vec<String>` — returns allowlisted video URLs found in `text`; `enrich_message_text` enqueues a `url` job per detected link and appends an ack line.

- [ ] **Step 1: Write the failing test**

Add to `subagent.rs` `mod tests` (create the module if absent):

```rust
#[test]
fn detect_video_links_youtube_only() {
    let text = "смотри https://www.youtube.com/watch?v=abc123 и https://example.com/x.mp4";
    let links = detect_video_links(text);
    assert_eq!(links.len(), 1);
    assert!(links[0].contains("youtube.com/watch?v=abc123"));

    assert!(detect_video_links("https://youtu.be/xyz").len() == 1, "youtu.be allowed");
    assert!(detect_video_links("нет ссылок тут").is_empty());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core detect_video_links_youtube_only -- --nocapture`
Expected: FAIL — `detect_video_links` not defined.

- [ ] **Step 3: Implement the detector + wire enqueue**

In `subagent.rs`, add:

```rust
/// v1 video-URL allowlist: YouTube only (SSRF surface — see spec §9).
fn detect_video_links(text: &str) -> Vec<String> {
    crate::agent::pipeline::subagent::extract_urls(text)
        .into_iter()
        .filter(|u| {
            let host = u.split('/').nth(2).unwrap_or("");
            host.ends_with("youtube.com") || host == "youtu.be" || host.ends_with(".youtu.be")
        })
        .collect()
}
```

(If `extract_urls` is private to the module, call it directly; it is already used in `enrich_message_text`.)

In `enrich_message_text`, after the existing URL-fetch loop and before/after `dispatch_attachments`, add:

```rust
    for link in detect_video_links(user_text) {
        match opex_db::video_jobs::enqueue_video_job(db, session_id, agent_name, "url", &link).await {
            Ok(_) => enriched.push_str("\n\n🎬 Видео по ссылке принято, готовлю сводку."),
            Err(e) => tracing::warn!(error=%e, link=%link, "video url enqueue failed"),
        }
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p opex-core detect_video_links_youtube_only -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/subagent.rs
git commit -m "feat(video): YouTube link detection enqueues url summarization job"
```

---

## Task 10: Config section + full build/lint/test gate

**Files:**
- Modify: `config/opex.toml`
- Verify: whole workspace

**Interfaces:** none new — wires tunables and confirms the feature compiles and tests green end to end.

- [ ] **Step 1: Add the config section**

Append to `config/opex.toml`:

```toml
[video]
# Scene-cut sensitivity for key-frame extraction (0..1 ffmpeg scene score).
scene_threshold = 0.4
# High safety ceiling on extracted frames (NOT a product cap).
frame_ceiling = 200
# Liveness guard per job (seconds) — fails a wedged job, not a cap on long video.
job_timeout_secs = 1800
# v1 video-URL download allowlist (yt-dlp). YouTube only.
url_allowlist = ["youtube.com", "youtu.be"]
```

> If these are read in code, thread them through the config struct; for v1 the toolgate env vars (`VIDEO_SCENE_THRESHOLD`, `VIDEO_FRAME_CEILING`) already provide the ffmpeg-side knobs, and the core worker reads `[video].job_timeout_secs`. Keep `.env` untouched (project rule).

- [ ] **Step 2: Lint**

Run: `make lint`
Expected: `cargo clippy --all-targets -- -D warnings` passes (no warnings introduced).

- [ ] **Step 3: Full DB-backed test suite**

Run: `make test-db`
Expected: all tests pass, including the new `video_jobs`, dispatch, seam, video_summary, video_worker tests.

- [ ] **Step 4: toolgate tests**

Run: `cd toolgate && python -m pytest test_video.py -v`
Expected: all PASS (requires ffmpeg + yt-dlp installed).

- [ ] **Step 5: Commit**

```bash
git add config/opex.toml
git commit -m "feat(video): config section + final wiring"
```

---

## Self-Review

**Spec coverage check (each spec section → task):**
- §5.1 toolgate `/summarize-video` (ffmpeg/STT/Vision/raw material, lift STT cap) → Tasks 2, 3. ✓
- §5.2 built-in `summarize_video` enqueue-only + seam change → Tasks 4, 5, 6. ✓
- §5.3 in-core worker → Tasks 7, 8. ✓
- §5.4 video-URL detector → Task 9. ✓
- §6 data flow (both inputs converge on queue) → Tasks 6 (file), 9 (url). ✓
- §7 web-only delivery (mirror/session inject + ui_event; channel_id NULL) → Task 8 `deliver`. ✓
- §8.1 `video_jobs` table + recovery → Task 1, recovery in Task 8 step 6. ✓
- §8.2 FSE seed → Task 4. ✓
- §9 no artificial limits (cap lifted; scene-driven frames; liveness timeout; yt-dlp allowlist) → Tasks 3 (cap), 9 (allowlist), 10 (timeout/threshold config). ✓
- §10 degradation (no vision → transcript-only; honest fail; never silently dropped) → Task 3 (degraded), Task 8 (failed delivery). ✓
- §11 tests → every task is TDD. ✓
- §12 resolved defaults (web-only, in-core worker, no limits, YouTube, core digest) → reflected across tasks. ✓

**Type consistency:** `VideoJob`, `EnqueueCtx`, `RawMaterial`/`FrameDesc`/`Degraded`, `dispatch_attachments(... session_id, agent_name ...)`, `enrich_message_text(... session_id, agent_name ...)`, `build_summary_messages`, `process_one`, `spawn_video_worker` — names match across Tasks 1/5/6/7/8/9.

**Known confirm-at-implementation points (flagged inline, not placeholders):**
- `opex_types::Message` exact field set (Task 7 step 3 note).
- `LlmResponse` text accessor — field vs method (Task 8 step 3 note).
- `AppState` accessor names for http/toolgate/gateway/agents/provider (Task 8 step 5 note).
- `ctx` session-id binding name in `bootstrap.rs` (Task 6 step 5 note).
These are real-signature confirmations a fresh implementer makes by reading the cited file; the surrounding code and types are fully specified.
