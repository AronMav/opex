"""Xiaomi MiMo Vision provider (multimodal chat).

Standard OpenAI-compatible vision shape: POST /v1/chat/completions with a
user message whose content contains a `text` part plus an `image_url` part
(data URI). Uses mimo-v2.5 or mimo-v2-omni as the multimodal backbone.
"""

import base64

import httpx

from providers.base import resolve_request_timeout


class MiMoVision:
    name = "Xiaomi MiMo Vision"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.xiaomimimo.com").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "mimo-v2.5"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def describe(self, http: httpx.AsyncClient, image_bytes: bytes,
                       content_type: str, prompt: str,
                       max_tokens: int = 2000) -> str:
        b64 = base64.b64encode(image_bytes).decode("ascii")
        data_url = f"data:{content_type};base64,{b64}"
        resp = await http.post(
            f"{self.base_url}/v1/chat/completions",
            headers={"Authorization": f"Bearer {self.api_key}"},
            json={
                "model": self.model,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": prompt},
                        {"type": "image_url", "image_url": {"url": data_url}},
                    ],
                }],
                "max_tokens": max_tokens,
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        return (
            data.get("choices", [{}])[0]
            .get("message", {})
            .get("content", "")
        ) or ""
