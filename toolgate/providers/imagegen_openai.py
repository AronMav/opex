"""OpenAI Image Generation provider (DALL-E 3 / GPT Image 1)."""

import base64

import httpx

from providers.base import resolve_request_timeout, join_openai_path


class OpenAIImageGen:
    name = "OpenAI Image Gen"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.openai.com/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "dall-e-3"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def generate(self, http: httpx.AsyncClient, prompt: str,
                       size: str = "1024x1024", model: str | None = None,
                       quality: str = "standard") -> bytes:
        mdl = model or self.model
        body: dict = {
            "model": mdl,
            "prompt": prompt,
            "size": size,
            "n": 1,
            "response_format": "b64_json",
        }
        if mdl.startswith("dall-e-3"):
            body["quality"] = quality

        resp = await http.post(
            join_openai_path(self.base_url, "/v1/images/generations"),
            headers={"Authorization": f"Bearer {self.api_key}"},
            json=body,
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        b64_data = data.get("data", [{}])[0].get("b64_json", "")
        if not b64_data:
            raise ValueError("No image data in response")
        return base64.b64decode(b64_data)
