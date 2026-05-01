"""Anthropic Claude Vision provider."""

import base64

import httpx

from providers.base import resolve_request_timeout


class AnthropicVision:
    name = "Anthropic Claude"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.anthropic.com").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "claude-sonnet-4-6"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def describe(self, http: httpx.AsyncClient, image_bytes: bytes,
                       content_type: str, prompt: str,
                       max_tokens: int = 2000) -> str:
        b64 = base64.b64encode(image_bytes).decode()

        # Map generic MIME to Anthropic-supported types
        media_type = content_type
        if media_type not in ("image/jpeg", "image/png", "image/gif", "image/webp"):
            media_type = "image/jpeg"

        resp = await http.post(
            f"{self.base_url}/v1/messages",
            headers={
                "x-api-key": self.api_key,
                "anthropic-version": "2023-06-01",
                "Content-Type": "application/json",
            },
            json={
                "model": self.model,
                "max_tokens": max_tokens,
                "messages": [{
                    "role": "user",
                    "content": [
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": b64,
                            },
                        },
                        {"type": "text", "text": prompt},
                    ],
                }],
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        content_blocks = data.get("content", [])
        for block in content_blocks:
            if block.get("type") == "text":
                return block.get("text", "")
        return ""
