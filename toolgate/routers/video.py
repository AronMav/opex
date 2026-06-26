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


async def _materialize_source(http, url: str) -> str:
    """Return a local video file path. Download from upload URL or via yt-dlp.

    NOTE: signature is (http, url) — a plain string. The router resolves the
    URL from the request body and passes it here, keeping this function simple
    to test/monkeypatch. yt-dlp URLs are identified by the `page_url` sentinel
    prefix stored as `yt-dlp::` in the URL string.
    """
    if url.startswith("yt-dlp::"):
        real_url = url[len("yt-dlp::"):]
        work_dir = tempfile.mkdtemp()
        return await download_video(real_url, work_dir)
    # Regular HTTP download (upload URL)
    work_dir = tempfile.mkdtemp()
    data, _ = await download_limited(http, url, max_bytes=None)
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
            video_path = await _materialize_source(http, source_url)
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
