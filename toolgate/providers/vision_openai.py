"""OpenAI Vision provider (GPT-4o)."""

import base64

import httpx

from providers.base import resolve_request_timeout


class OpenAIVision:
    name = "OpenAI Vision"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.openai.com/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "gpt-4o"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def describe(self, http: httpx.AsyncClient, image_bytes: bytes,
                       content_type: str, prompt: str,
                       max_tokens: int = 2000) -> str:
        b64 = base64.b64encode(image_bytes).decode()
        data_url = f"data:{content_type};base64,{b64}"

        resp = await http.post(
            f"{self.base_url}/chat/completions",
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
        return data.get("choices", [{}])[0].get("message", {}).get("content", "")
