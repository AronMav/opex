"""Deepgram STT provider."""

import httpx

from providers.base import resolve_request_timeout


class DeepgramSTT:
    name = "Deepgram"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.deepgram.com/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "nova-3"
        opts = options or {}
        self.detect_language = opts.get("detect_language", False)
        self.punctuate = opts.get("punctuate", True)
        self.smart_format = opts.get("smart_format", True)
        self._request_timeout = resolve_request_timeout(opts)

    async def transcribe(self, http: httpx.AsyncClient, audio_bytes: bytes,
                         filename: str, language: str,
                         model: str | None = None) -> str:
        params = {
            "model": model or self.model,
            "punctuate": str(self.punctuate).lower(),
            "smart_format": str(self.smart_format).lower(),
        }
        if self.detect_language:
            params["detect_language"] = "true"
        else:
            params["language"] = language

        resp = await http.post(
            f"{self.base_url}/listen",
            params=params,
            headers={
                "Authorization": f"Token {self.api_key}",
                "Content-Type": "audio/ogg",
            },
            content=audio_bytes,
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        channels = data.get("results", {}).get("channels", [])
        if channels:
            alts = channels[0].get("alternatives", [])
            if alts:
                return alts[0].get("transcript", "")
        return ""
