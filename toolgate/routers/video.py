"""Video summarization orchestrator. Heavy media work for FSE summarize_video.
Returns RAW MATERIAL (transcript + frame descriptions); the final LLM digest is
built in opex-core, not here (toolgate has no text-LLM)."""

import asyncio
import json
import logging
import os
import sys
import tempfile
from urllib.parse import urlparse

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from video_helpers import extract_audio, extract_scene_frames, extract_uniform_frames, download_video, _run, _cookie_args_async, _cleanup_cookie_args

# Hostnames trusted for video_url (always a Core gateway upload URL — localhost only).
_LOOPBACK_HOSTS = {"localhost", "127.0.0.1", "0.0.0.0", "::1"}

log = logging.getLogger("toolgate.video")

router = APIRouter(tags=["video"])

# Scene cut sensitivity (0..1 ffmpeg scene score). Kept for backward compat /
# direct callers of extract_scene_frames; not used by summarize_video anymore.
SCENE_THRESHOLD = float(os.environ.get("VIDEO_SCENE_THRESHOLD", "0.4"))
FRAME_CEILING = int(os.environ.get("VIDEO_FRAME_CEILING", "200"))

FRAME_VISION_CONCURRENCY = 4

# VIDEO_FRAME_CANDIDATES — how many uniform frames to extract as vision candidates.
# Higher → better coverage at the cost of more ffmpeg seek operations.
VIDEO_FRAME_CANDIDATES = int(os.environ.get("VIDEO_FRAME_CANDIDATES", "54"))

# VIDEO_NOTE_MAX_FRAMES — how many top-scored frames to keep in the final output.
# Must be ≤ VIDEO_FRAME_CANDIDATES.  Frames are re-sorted by timestamp after pick.
VIDEO_NOTE_MAX_FRAMES = int(os.environ.get("VIDEO_NOTE_MAX_FRAMES", "24"))


class SummarizeVideoRequest(BaseModel):
    video_url: str | None = None   # localhost upload URL (file source)
    page_url: str | None = None    # link for yt-dlp (url source)
    language: str = "ru"
    title: str | None = None


async def _materialize_source(http, url: str, work_dir: str) -> str:
    """Return a local video file path. Download from upload URL or via yt-dlp.

    NOTE: signature is (http, url, work_dir) — the caller provides the scratch
    directory. The router resolves the URL from the request body and passes it
    here, keeping this function simple to test/monkeypatch. yt-dlp URLs are
    identified by the `page_url` sentinel prefix stored as `yt-dlp::` in the
    URL string.
    """
    if url.startswith("yt-dlp::"):
        real_url = url[len("yt-dlp::"):]
        return await download_video(real_url, work_dir)
    # Regular HTTP download (upload URL — MUST be a localhost Core gateway URL).
    # Inverted allowlist: only loopback is permitted here to prevent SSRF.
    # page_url / yt-dlp branch is separately guarded by the YouTube allowlist
    # enforced in opex-core (detect_video_links) and the http/https scheme check
    # in download_video.
    parsed = urlparse(url)
    if parsed.scheme not in ("http", "https") or parsed.hostname not in _LOOPBACK_HOSTS:
        raise ValueError("video_url must be a localhost gateway URL")
    # Loopback host already validated above. download_limited would (a) block
    # loopback via validate_url_ssrf and (b) TypeError on max_bytes=None, so fetch
    # the already-trusted upload URL directly. No size cap per the no-limits design.
    resp = await http.get(url, follow_redirects=False)
    resp.raise_for_status()
    data = resp.content
    path = os.path.join(work_dir, "upload.mp4")
    with open(path, "wb") as f:
        f.write(data)
    return path


@router.post("/summarize-video")
async def summarize_video(body: SummarizeVideoRequest, request: Request):
    http = request.app.state.http_client
    registry = request.app.state.registry
    degraded = {"stt": False, "vision": False}

    # Resolve the source URL before entering the temp dir so _materialize_source
    # can be monkeypatched as a simple (http, url) callable in tests.
    if body.page_url:
        source_url = f"yt-dlp::{body.page_url}"
    elif body.video_url:
        source_url = body.video_url
    else:
        return JSONResponse(status_code=422, content={"error": "either video_url or page_url is required"})

    with tempfile.TemporaryDirectory() as work_dir:
        try:
            video_path = await _materialize_source(http, source_url, work_dir)
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

        # ── Uniform-candidate frames → Vision scoring → top-N selection ────────
        frames_out: list[dict] = []
        try:
            candidates = await extract_uniform_frames(video_path, VIDEO_FRAME_CANDIDATES)
        except Exception as e:
            candidates = []
            log.warning("uniform frame extract failed (continuing transcript-only): %s", e)

        vision = await registry.aget_active("vision")

        if not candidates:
            pass  # nothing to describe; frames_out stays empty
        elif vision is None:
            # Vision unavailable → no descriptions; screenshots are no longer
            # embedded, so there is nothing to add from frames.
            degraded["vision"] = True
            frames_out = []
        else:
            # Vision-scoring: evaluate every candidate, pick top-N, re-sort by time.
            sem = asyncio.Semaphore(FRAME_VISION_CONCURRENCY)
            score_prompt = (
                "Оцени кадр из обучающего видео (урок в программе). "
                "Верни ТОЛЬКО JSON одной строкой: "
                '{\"score\": <0-10>, \"description\": \"<краткое описание на русском>\"}. '
                "score=0-2 ОБЯЗАТЕЛЬНО если кадр размытый, смазанный, с motion-blur, "
                "в движении, на переходе или анимации между сценами, нечёткий, или это "
                "говорящий человек на камеру / заставка / пустой экран. Высокий score (8-10) "
                "ТОЛЬКО для ЧЁТКИХ СТАТИЧНЫХ кадров с хорошо читаемым контентом интерфейса "
                "(плагин, окно настроек, панель, текст на экране)."
            )

            async def score_frame(ts: float, jpeg: bytes):
                async with sem:
                    raw = ""
                    try:
                        raw = await vision.describe(http, jpeg, "image/jpeg", score_prompt)
                        # Extract JSON between first { and last }
                        start = raw.find("{")
                        end = raw.rfind("}") + 1
                        if start >= 0 and end > start:
                            parsed = json.loads(raw[start:end])
                            sc = max(0, min(10, int(parsed.get("score", 5))))
                            desc = str(parsed.get("description", "")).strip()
                        else:
                            raise ValueError("no JSON object in response")
                    except Exception as e:
                        log.warning("frame score failed at %.1fs: %s", ts, e)
                        sc = 5
                        desc = raw[:200] if raw else ""
                    return (ts, sc, desc)

            scored = await asyncio.gather(*(score_frame(ts, j) for ts, j in candidates))

            # Sort by score descending, take top-N, then re-sort by timestamp.
            top = sorted(scored, key=lambda x: x[1], reverse=True)[:VIDEO_NOTE_MAX_FRAMES]
            top_by_time = sorted(top, key=lambda x: x[0])

            frames_out = [
                {"timestamp": ts, "description": desc}
                for ts, _sc, desc in top_by_time
            ]

        # Probe duration (best-effort, non-fatal).
        duration = 0.0
        try:
            code, out, _ = await _run(
                "ffprobe", "-v", "error", "-show_entries", "format=duration",
                "-of", "default=nw=1:nk=1", video_path,
            )
            if code == 0:
                duration = float(out.decode().strip() or 0.0)
        except Exception:
            pass

        # Resolve title: use explicit body.title; for page_url probe yt-dlp.
        resolved_title = body.title or ""
        # http/https scheme check (rejects '-'-prefixed flag-smuggling + file:/ftp:)
        # and '--' terminates yt-dlp option parsing so the URL can't be read as a flag.
        if not resolved_title and body.page_url and body.page_url.startswith(("http://", "https://")):
            cookie_args = await _cookie_args_async()
            try:
                code, out, _ = await _run(
                    sys.executable, "-m", "yt_dlp", "--js-runtimes", "deno", *cookie_args,
                    "--print", "%(title)s", "--skip-download", "--", body.page_url,
                )
                if code == 0:
                    resolved_title = out.decode(errors="ignore").strip()
            except Exception:
                pass
            finally:
                _cleanup_cookie_args(cookie_args)

        return {
            "title": resolved_title,
            "duration": duration,
            "transcript": transcript,
            "frames": frames_out,
            "degraded": degraded,
        }
