"""Video summarization orchestrator. Heavy media work for FSE summarize_video.
Returns RAW MATERIAL (transcript + frame descriptions); the final LLM digest is
built in opex-core, not here (toolgate has no text-LLM)."""

import asyncio
import base64
import json
import logging
import os
import re
import sys
import tempfile
from urllib.parse import parse_qs, urlparse

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from video_helpers import extract_audio, extract_scene_frames, extract_uniform_frames, download_video, _run

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

# ── Raw-material cache ───────────────────────────────────────────────────────
# Cache the *raw material* (transcript + scored frames + title + duration) keyed
# by the stable YouTube video-id so repeated requests on the same video skip the
# ~6-minute download+STT+vision pass. Two motivations: (a) avoid hammering
# YouTube into a 429 rate-limit while iterating, (b) test the downstream LLM
# digest fast without a full re-run. Only `page_url` (yt-dlp) sources are
# cacheable — `video_url` uploads are one-time signed URLs and are never cached.
VIDEO_CACHE_DIR = os.environ.get("VIDEO_CACHE_DIR", "/tmp/opex_video_cache")

# Standard YouTube video-id: exactly 11 chars of [A-Za-z0-9_-]. Strict validation
# is a security boundary — the id is used as a cache filename, so anything that
# isn't a clean 11-char id (e.g. a path-traversal attempt) yields None and the
# request falls back to a normal full run with no cache filesystem touch.
_YT_VIDEO_ID_RE = re.compile(r"^[A-Za-z0-9_-]{11}$")


def _youtube_video_id(url: str) -> str | None:
    """Extract a stable YouTube video-id from a watch / youtu.be URL.

    Returns the 11-char id, or None if the URL has no recognisable id or the
    candidate fails the strict `[A-Za-z0-9_-]{11}` shape check. The strictness
    is deliberate: the id becomes a filename, so a loose match would open a
    path-traversal hole (e.g. `?v=../../etc/passwd`).
    """
    if not url or not isinstance(url, str):
        return None
    try:
        parsed = urlparse(url)
    except Exception:
        return None

    candidate: str | None = None
    host = (parsed.hostname or "").lower()
    if host == "youtu.be":
        # https://youtu.be/<id>  → first non-empty path segment.
        candidate = parsed.path.lstrip("/").split("/", 1)[0]
    else:
        # watch?v=<id> (also covers youtube.com / m.youtube.com / music.*).
        qs = parse_qs(parsed.query)
        v = qs.get("v")
        if v:
            candidate = v[0]

    if candidate and _YT_VIDEO_ID_RE.match(candidate):
        return candidate
    return None


def _cache_path(video_id: str) -> str:
    """Filesystem path for a cached video-id. Caller guarantees video_id is a
    validated 11-char id (see _youtube_video_id), so no traversal is possible."""
    return os.path.join(VIDEO_CACHE_DIR, f"{video_id}.json")


def _read_cache(video_id: str) -> dict | None:
    """Return the cached response dict for video_id, or None if absent/unreadable.
    Never raises — a corrupt or missing cache simply triggers a full run."""
    path = _cache_path(video_id)
    try:
        if not os.path.exists(path):
            return None
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
        if isinstance(data, dict):
            return data
        log.warning("video cache file %s is not a JSON object — ignoring", path)
        return None
    except Exception as e:
        log.warning("video cache read failed for %s: %s", video_id, e)
        return None


def _write_cache(video_id: str, payload: dict) -> None:
    """Persist payload as the cache for video_id. Never raises — a write failure
    is logged and the (already-computed) result is returned uncached."""
    try:
        os.makedirs(VIDEO_CACHE_DIR, exist_ok=True)
        path = _cache_path(video_id)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(payload, f, ensure_ascii=False)
    except Exception as e:
        log.warning("video cache write failed for %s: %s", video_id, e)


class SummarizeVideoRequest(BaseModel):
    video_url: str | None = None   # localhost upload URL (file source)
    page_url: str | None = None    # link for yt-dlp (url source)
    language: str = "ru"
    title: str | None = None
    reuse_cache: bool = True       # url sources: serve cached raw material if present


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
    #
    # video_id is the cache key — only set for page_url (yt-dlp) sources with a
    # parseable, strictly-validated YouTube id. video_url (upload) sources are
    # one-time signed URLs and are never cached.
    video_id: str | None = None
    if body.page_url:
        source_url = f"yt-dlp::{body.page_url}"
        video_id = _youtube_video_id(body.page_url)
    elif body.video_url:
        source_url = body.video_url
    else:
        return JSONResponse(status_code=422, content={"error": "either video_url or page_url is required"})

    # ── Cache hit: serve raw material immediately (no download/STT/vision) ──────
    if video_id and body.reuse_cache:
        cached = _read_cache(video_id)
        if cached is not None:
            log.info("video cache hit: %s", video_id)
            return JSONResponse(status_code=200, content=cached)

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
            # Degraded: vision unavailable — even-spread fallback (no scoring).
            degraded["vision"] = True
            step = max(1, len(candidates) / VIDEO_NOTE_MAX_FRAMES)
            selected = [candidates[int(i * step)] for i in range(min(VIDEO_NOTE_MAX_FRAMES, len(candidates)))]
            frames_out = [
                {
                    "timestamp": ts,
                    "description": "",
                    "image_b64": base64.b64encode(jpeg).decode("ascii"),
                }
                for ts, jpeg in selected
            ]
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
                    b64 = base64.b64encode(jpeg).decode("ascii")
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
                    return (ts, sc, desc, b64)

            scored = await asyncio.gather(*(score_frame(ts, j) for ts, j in candidates))

            # Sort by score descending, take top-N, then re-sort by timestamp.
            top = sorted(scored, key=lambda x: x[1], reverse=True)[:VIDEO_NOTE_MAX_FRAMES]
            top_by_time = sorted(top, key=lambda x: x[0])

            frames_out = [
                {"timestamp": ts, "description": desc, "image_b64": b64}
                for ts, _sc, desc, b64 in top_by_time
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
            try:
                code, out, _ = await _run(
                    sys.executable, "-m", "yt_dlp", "--js-runtimes", "node",
                    "--print", "%(title)s", "--skip-download", "--", body.page_url,
                )
                if code == 0:
                    resolved_title = out.decode(errors="ignore").strip()
            except Exception:
                pass

        result = {
            "title": resolved_title,
            "duration": duration,
            "transcript": transcript,
            "frames": frames_out,
            "degraded": degraded,
        }

        # Persist raw material for url sources with a valid video-id so the next
        # request can short-circuit the full pass. Best-effort: a cache write
        # failure never affects the returned result.
        if video_id:
            _write_cache(video_id, result)

        return result
