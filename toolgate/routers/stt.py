"""STT (Speech-to-Text) endpoints."""

import logging

from fastapi import APIRouter, UploadFile, File, Form, Request, Depends
from fastapi.responses import JSONResponse
from pydantic import BaseModel
from typing import Optional

import httpx

from dependencies import require_provider
from helpers import download_limited, check_upload_size, log_provider
from audio_trim import trim_silence
from transcript import strip_transcript_timecodes

log = logging.getLogger("toolgate.stt")

STT_MAX_BYTES = 25 * 1024 * 1024  # 25 MB (legacy Whisper API limit; kept for cloud providers)

# A provider may declare a hard per-file byte cap (cloud APIs). Local Whisper
# has none → no cap. Default: no cap unless the provider sets `max_bytes`.
def _provider_cap(provider) -> int | None:
    return getattr(provider, "max_bytes", None)

router = APIRouter(tags=["stt"])


@router.post("/transcribe")
async def transcribe(
    request: Request,
    file: UploadFile = File(...),
    model: str = Form(default=None),
    language: str = Form(default="ru"),
    provider=Depends(require_provider("stt")),
):
    log_provider(log, provider)
    audio_bytes = await file.read()

    cap = _provider_cap(provider)
    if cap is not None:
        size_err = check_upload_size(audio_bytes, cap, "Audio file")
        if size_err:
            return size_err

    # Best-effort: strip leading/trailing silence before the (paid) STT call.
    _name = file.filename or "audio.ogg"
    _in_ext = _name.rsplit(".", 1)[-1].lower() if "." in _name else "ogg"
    audio_bytes, _out_ext = await trim_silence(audio_bytes, _in_ext)

    try:
        text = await provider.transcribe(
            request.app.state.http_client, audio_bytes,
            f"audio.{_out_ext}", language, model,
        )
        text = strip_transcript_timecodes(text)
        return {"text": text}
    except httpx.HTTPStatusError as e:
        return JSONResponse(status_code=e.response.status_code,
                            content={"error": f"STT error: {e.response.text}"})
    except Exception as e:
        return JSONResponse(status_code=502, content={"error": f"STT error: {e}"})


class TranscribeUrlRequest(BaseModel):
    audio_url: str
    language: Optional[str] = "ru"
    model: Optional[str] = None


@router.post("/transcribe-url")
async def transcribe_url(
    body: TranscribeUrlRequest,
    request: Request,
    provider=Depends(require_provider("stt")),
):
    log_provider(log, provider)
    http = request.app.state.http_client
    cap = _provider_cap(provider)
    try:
        audio_bytes, _ = await download_limited(http, body.audio_url, max_bytes=cap or STT_MAX_BYTES)
    except Exception as e:
        return JSONResponse(status_code=502, content={"error": f"Failed to download audio: {e}"})

    filename = body.audio_url.split("/")[-1].split("?")[0] or "audio.ogg"
    _in_ext = filename.rsplit(".", 1)[-1].lower() if "." in filename else "ogg"
    audio_bytes, _out_ext = await trim_silence(audio_bytes, _in_ext)

    try:
        text = await provider.transcribe(
            http, audio_bytes, f"audio.{_out_ext}", body.language or "ru", body.model,
        )
        return {"text": text}
    except httpx.HTTPStatusError as e:
        return JSONResponse(status_code=e.response.status_code,
                            content={"error": f"STT error: {e.response.text}"})
    except Exception as e:
        return JSONResponse(status_code=502, content={"error": f"STT error: {e}"})
