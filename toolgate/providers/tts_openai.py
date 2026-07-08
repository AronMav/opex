"""OpenAI TTS provider."""

import httpx

from providers.base import resolve_request_timeout, join_openai_path


class OpenAITTS:
    name = "OpenAI TTS"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.openai.com/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "gpt-4o-mini-tts"
        opts = options or {}
        self.default_voice = opts.get("voice", "alloy")
        self._request_timeout = resolve_request_timeout(opts)

    async def synthesize(self, http: httpx.AsyncClient, text: str,
                         voice: str, model: str | None = None,
                         response_format: str = "mp3",
                         registry=None) -> bytes:
        resp = await http.post(
            join_openai_path(self.base_url, "/v1/audio/speech"),
            headers={"Authorization": f"Bearer {self.api_key}"},
            json={
                "model": model or self.model,
                "input": text,
                "voice": voice or self.default_voice,
                "response_format": response_format,
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        return resp.content
