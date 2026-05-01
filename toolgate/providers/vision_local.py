"""Ollama vision provider (local or cloud API)."""

import base64

import httpx

from providers.base import resolve_request_timeout


class OllamaVision:
    name = "Ollama Vision"

    def __init__(self, base_url: str, api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "qwen3.5"
        opts = options or {}
        self.max_tokens = opts.get("max_tokens", 2000)
        # Detect cloud vs local: cloud uses /api/chat, local uses /v1/chat/completions
        self.is_cloud = "ollama.com" in self.base_url
        self._request_timeout = resolve_request_timeout(opts)

    async def describe(self, http: httpx.AsyncClient, image_bytes: bytes,
                       content_type: str, prompt: str,
                       max_tokens: int = 2000) -> str:
        b64 = base64.b64encode(image_bytes).decode()

        headers = {}
        if self.api_key:
            headers["Authorization"] = f"Bearer {self.api_key}"

        if self.is_cloud:
            return await self._describe_native(http, headers, b64, prompt)
        return await self._describe_openai(http, headers, b64, content_type, prompt, max_tokens)

    async def _describe_native(self, http: httpx.AsyncClient, headers: dict,
                               b64: str, prompt: str) -> str:
        """Ollama native API (/api/chat) — used by Ollama Cloud."""
        # Strip /v1 suffix if present (cloud base_url should be https://ollama.com)
        base = self.base_url.removesuffix("/v1")
        resp = await http.post(
            f"{base}/api/chat",
            headers=headers,
            json={
                "model": self.model,
                "messages": [{
                    "role": "user",
                    "content": prompt,
                    "images": [b64],
                }],
                "stream": False,
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        return data.get("message", {}).get("content", "")

    async def _describe_openai(self, http: httpx.AsyncClient, headers: dict,
                               b64: str, content_type: str, prompt: str,
                               max_tokens: int) -> str:
        """OpenAI-compatible API (/v1/chat/completions) — used by local Ollama."""
        data_url = f"data:{content_type};base64,{b64}"
        resp = await http.post(
            f"{self.base_url}/chat/completions",
            headers=headers,
            json={
                "model": self.model,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": prompt},
                        {"type": "image_url", "image_url": {"url": data_url}},
                    ],
                }],
                "max_tokens": max_tokens or self.max_tokens,
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        return data.get("choices", [{}])[0].get("message", {}).get("content", "")
