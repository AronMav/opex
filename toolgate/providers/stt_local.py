"""Local faster-whisper STT provider (OpenAI-compatible API)."""

import httpx

from providers.base import resolve_request_timeout


class LocalWhisperSTT:
    name = "Local Whisper"

    def __init__(self, base_url: str, api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = base_url.rstrip("/")
        self.model = model or "Systran/faster-whisper-large-v3"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def transcribe(self, http: httpx.AsyncClient, audio_bytes: bytes,
                         filename: str, language: str,
                         model: str | None = None) -> str:
        resp = await http.post(
            f"{self.base_url}/audio/transcriptions",
            files={"file": (filename, audio_bytes, "audio/ogg")},
            data={"model": model or self.model, "language": language},
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        return resp.json().get("text", "")
