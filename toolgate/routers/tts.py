"""TTS (Text-to-Speech) endpoints."""

import logging

from fastapi import APIRouter, Request, Depends
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel
from typing import Optional

import httpx

from dependencies import require_provider
from helpers import log_provider

log = logging.getLogger("toolgate.tts")

router = APIRouter(tags=["tts"])

_AUDIO_MEDIA_TYPES = {
    "mp3": "audio/mpeg",
    "opus": "audio/ogg",
    "aac": "audio/aac",
    "flac": "audio/flac",
    "wav": "audio/wav",
}


def _audio_media_type(fmt: str) -> str:
    """Map response_format to MIME type."""
    return _AUDIO_MEDIA_TYPES.get(fmt, "audio/mpeg")


@router.get("/audio/voices")
async def list_voices(
    request: Request,
    provider=Depends(require_provider("tts")),
):
    """List available voices from the TTS provider."""
    http = request.app.state.http_client

    # Prefer a native list_voices() method on the provider instance
    if hasattr(provider, "list_voices"):
        try:
            return await provider.list_voices(http)
        except Exception as e:
            log.warning("list_voices() failed on %s: %s", provider.name, e)
            return {"voices": [], "note": "Voice listing unavailable for this provider"}

    # Fallback: try proxying to base_url if provider exposes one (e.g. local Qwen TTS server)
    base_url = getattr(provider, "base_url", None)
    if base_url:
        try:
            resp = await http.get(f"{base_url}/v1/audio/voices", timeout=5.0)
            resp.raise_for_status()
            return resp.json()
        except Exception as e:
            log.warning("Failed to fetch voices from %s: %s", base_url, e)

    return {"voices": [], "note": "This provider does not support voice listing"}


class OpenAISpeechRequest(BaseModel):
    model: Optional[str] = None
    input: str
    voice: Optional[str] = None
    response_format: Optional[str] = "mp3"


@router.post("/v1/audio/speech")
async def openai_speech(
    body: OpenAISpeechRequest,
    request: Request,
    provider=Depends(require_provider("tts")),
):
    """OpenAI-compatible TTS. The provider performs normalization internally
    (via its configured normalize_provider_id) so the router stays neutral."""
    log_provider(log, provider)
    http = request.app.state.http_client
    text = body.input
    fmt = body.response_format or "mp3"
    voice = body.voice or request.headers.get("x-opex-voice") or ""
    try:
        audio_bytes = await provider.synthesize(
            http, text,
            voice, body.model, fmt,
            registry=request.app.state.registry,
        )
        return Response(content=audio_bytes, media_type=_audio_media_type(fmt))
    except httpx.HTTPStatusError as e:
        return JSONResponse(status_code=e.response.status_code,
                            content={"error": f"TTS error: {e.response.text}"})
    except Exception as e:
        return JSONResponse(status_code=502, content={"error": f"TTS error: {e}"})
