"""Qwen2-VL Vision provider via DashScope API."""

import base64

import httpx

from providers.base import resolve_request_timeout


class QwenVision:
    name = "Qwen2-VL"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://dashscope.aliyuncs.com/api/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "qwen-vl-max-latest"
        self.options = options or {}
        self._request_timeout = resolve_request_timeout(self.options)

    async def describe(self, http: httpx.AsyncClient, image_bytes: bytes,
                       content_type: str, prompt: str,
                       max_tokens: int = 2000) -> str:
        b64_image = base64.b64encode(image_bytes).decode()

        messages = [
            {
                "role": "user",
                "content": [
                    {"image": f"data:{content_type};base64,{b64_image}"},
                    {"text": prompt},
                ]
            }
        ]

        payload = {
            "model": self.model,
            "input": {"messages": messages},
            "parameters": {
                "max_tokens": max_tokens,
                "temperature": self.options.get("temperature", 0.7),
                "top_p": self.options.get("top_p", 0.9),
                "result_format": "message",
            },
        }

        resp = await http.post(
            f"{self.base_url}/services/aigc/multimodal-generation/generation",
            json=payload,
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()

        try:
            content = data["output"]["choices"][0]["message"]["content"]
            if isinstance(content, list):
                return content[0].get("text", "")
            return str(content)
        except (KeyError, IndexError):
            if "output" in data and "text" in data["output"]:
                return data["output"]["text"]
            raise Exception(f"Unexpected DashScope response: {data}")
