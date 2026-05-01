"""fal.ai ImageGen provider — fast FLUX image generation API."""

import asyncio

import httpx

from providers.base import ImageGenProvider, resolve_request_timeout
from helpers import validate_url_ssrf


class FalImageGen(ImageGenProvider):
    """fal.ai — fast FLUX and SDXL image generation.

    API Docs: https://fal.ai/docs
    """

    name = "fal.ai"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = base_url or "https://queue.fal.run/fal-ai"
        self.api_key = api_key or ""
        self.model = model or "flux/schnell"
        self.options = options or {}
        self._request_timeout = resolve_request_timeout(self.options)

    async def generate(self, http: httpx.AsyncClient, prompt: str,
                       size: str = "1024x1024", model: str | None = None,
                       quality: str = "standard") -> bytes:
        width, height = map(int, size.split("x"))
        model_id = model or self.model

        payload: dict = {
            "prompt": prompt,
            "image_size": {"width": width, "height": height},
        }
        if "seed" in self.options:
            payload["seed"] = self.options["seed"]
        if quality == "high":
            # schnell max 12 steps; pro/dev models support more
            is_schnell = "schnell" in model_id
            payload["num_inference_steps"] = 12 if is_schnell else 28

        headers = {"Content-Type": "application/json"}
        if self.api_key:
            headers["Authorization"] = f"Key {self.api_key}"

        # Submit to queue
        resp = await http.post(
            f"{self.base_url}/{model_id}",
            json=payload,
            headers=headers,
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()

        # Async queue — poll for result using URLs from response
        if "request_id" in data:
            response_url = data.get("response_url", "")
            status_url = data.get("status_url", "")

            if not response_url or not status_url:
                # Fallback to constructed URLs
                request_id = data["request_id"]
                response_url = f"{self.base_url}/{model_id}/requests/{request_id}"
                status_url = f"{response_url}/status"

            for _ in range(60):  # max ~60s
                await asyncio.sleep(1)
                status_resp = await http.get(status_url, headers=headers, timeout=self._request_timeout)
                status_resp.raise_for_status()
                status_data = status_resp.json()
                status = status_data.get("status")

                if status == "COMPLETED":
                    result_resp = await http.get(response_url, headers=headers, timeout=self._request_timeout)
                    result_resp.raise_for_status()
                    result_data = result_resp.json()
                    images = result_data.get("images", [])
                    if images:
                        image_url = images[0]["url"]
                        validate_url_ssrf(image_url)
                        img_resp = await http.get(image_url, timeout=self._request_timeout)
                        img_resp.raise_for_status()
                        return img_resp.content
                    raise Exception("fal.ai returned no images")
                elif status in ("FAILED", "CANCELLED"):
                    raise Exception(f"fal.ai {status}: {status_data.get('error', 'unknown')}")

            raise Exception("fal.ai image generation timed out after 60s")

        # Synchronous response
        if "images" in data:
            image_url = data["images"][0]["url"]
            validate_url_ssrf(image_url)
            img_resp = await http.get(image_url, timeout=self._request_timeout)
            img_resp.raise_for_status()
            return img_resp.content

        raise Exception(f"Unexpected fal.ai response: {data}")
