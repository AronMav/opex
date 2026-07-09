"""Google Gemini STT provider."""

import base64

import httpx

from providers.base import resolve_request_timeout


class GoogleSTT:
    name = "Google Gemini"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://generativelanguage.googleapis.com/v1beta").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "gemini-2.0-flash"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def transcribe(self, http: httpx.AsyncClient, audio_bytes: bytes,
                         filename: str, language: str,
                         model: str | None = None) -> str:
        b64 = base64.b64encode(audio_bytes).decode()
        mdl = model or self.model
        resp = await http.post(
            f"{self.base_url}/models/{mdl}:generateContent",
            # F053: send the key in a header, NOT ?key= — the query form lands
            # verbatim in httpx OTel span url.full attributes (leaked to Jaeger).
            headers={"x-goog-api-key": self.api_key},
            json={
                "contents": [{
                    "parts": [
                        {"text": f"Transcribe this audio. Language: {language}. Return only the transcription text."},
                        {"inline_data": {"mime_type": "audio/ogg", "data": b64}},
                    ]
                }],
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        candidates = data.get("candidates", [])
        if candidates:
            parts = candidates[0].get("content", {}).get("parts", [])
            if parts:
                return parts[0].get("text", "")
        return ""
