"""OpenRouter STT provider.

OpenRouter's /v1/audio/transcriptions deviates from OpenAI's multipart
form-data: it accepts a JSON body with the audio base64-encoded inside
`input_audio.data` (raw bytes, NOT a data URI). Model slugs include
`openai/whisper-large-v3`, `openai/whisper-1`, `openai/gpt-4o-mini-transcribe`,
groq's Whisper, Google Chirp 3.
"""

import base64

import httpx

from providers.base import resolve_request_timeout


def _format_from_filename(filename: str) -> str:
    lower = (filename or "").lower()
    for ext in ("mp3", "wav", "flac", "m4a", "ogg", "webm", "aac"):
        if lower.endswith(f".{ext}"):
            return ext
    return "mp3"


class OpenRouterSTT:
    name = "OpenRouter Whisper"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://openrouter.ai/api/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "openai/whisper-large-v3"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def transcribe(self, http: httpx.AsyncClient, audio_bytes: bytes,
                         filename: str, language: str,
                         model: str | None = None) -> str:
        b64 = base64.b64encode(audio_bytes).decode("ascii")
        body: dict = {
            "model": model or self.model,
            "input_audio": {
                "data": b64,
                "format": _format_from_filename(filename),
            },
        }
        if language:
            body["language"] = language
        resp = await http.post(
            f"{self.base_url}/audio/transcriptions",
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
            json=body,
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        return resp.json().get("text", "") or ""
