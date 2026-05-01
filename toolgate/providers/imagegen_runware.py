"""Runware ImageGen provider — FLUX, Stable Diffusion, etc."""

import uuid

import httpx

from providers.base import ImageGenProvider, resolve_request_timeout
from helpers import validate_url_ssrf


class RunwareImageGen(ImageGenProvider):
    name = "Runware"

    def __init__(self, base_url: str | None = None, api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.runware.ai/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "runware:100@1"
        self.options = options or {}
        self._request_timeout = resolve_request_timeout(self.options)

    async def generate(self, http: httpx.AsyncClient, prompt: str,
                       size: str = "1024x1024", model: str | None = None,
                       quality: str = "standard") -> bytes:
        width, height = map(int, size.split("x"))

        payload = [
            {
                "taskType": "imageInference",
                "taskUUID": str(uuid.uuid4()),
                "positivePrompt": prompt,
                "model": model or self.model,
                "width": width,
                "height": height,
                "numberResults": 1,
                "outputType": "URL",
            }
        ]

        resp = await http.post(
            self.base_url,
            json=payload,
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {self.api_key}",
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()

        data = resp.json()
        items = data.get("data", data) if isinstance(data, dict) else data
        if isinstance(items, list) and items:
            image_url = items[0].get("imageURL", "")
        else:
            raise Exception(f"Unexpected Runware response: {data}")

        validate_url_ssrf(image_url)
        img_resp = await http.get(image_url, timeout=self._request_timeout)
        img_resp.raise_for_status()
        return img_resp.content
