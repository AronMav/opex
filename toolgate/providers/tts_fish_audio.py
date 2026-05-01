"""Fish Audio TTS provider — streaming TTS with voice cloning."""

import httpx

from providers.base import resolve_request_timeout


class FishAudioTTS:
    name = "Fish Audio"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.fish.audio").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "s1"
        opts = options or {}
        self.reference_id = opts.get("reference_id")
        self.latency = opts.get("latency", "normal")
        self._request_timeout = resolve_request_timeout(opts)

    async def synthesize(self, http: httpx.AsyncClient, text: str,
                         voice: str, model: str | None = None,
                         response_format: str = "mp3",
                         registry=None) -> bytes:
        voice_id = voice or self.reference_id or ""
        engine = model or self.model

        payload: dict = {
            "text": text,
            "format": response_format,
            "latency": self.latency,
        }
        if voice_id:
            payload["reference_id"] = voice_id

        resp = await http.post(
            f"{self.base_url}/v1/tts",
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
                "model": engine,
            },
            json=payload,
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        return resp.content
