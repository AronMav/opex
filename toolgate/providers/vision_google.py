"""Google Gemini Vision provider."""

import base64

import httpx

from providers.base import resolve_request_timeout


class GoogleVision:
    name = "Google Gemini Vision"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://generativelanguage.googleapis.com/v1beta").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "gemini-2.0-flash"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def describe(self, http: httpx.AsyncClient, image_bytes: bytes,
                       content_type: str, prompt: str,
                       max_tokens: int = 2000) -> str:
        b64 = base64.b64encode(image_bytes).decode()

        resp = await http.post(
            f"{self.base_url}/models/{self.model}:generateContent",
            # F053: key via header, not ?key= (avoids leaking it into OTel span
            # url.full attributes exported to the trace collector).
            headers={"x-goog-api-key": self.api_key},
            json={
                "contents": [{
                    "parts": [
                        {"text": prompt},
                        {"inline_data": {"mime_type": content_type, "data": b64}},
                    ]
                }],
                "generationConfig": {"maxOutputTokens": max_tokens},
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
