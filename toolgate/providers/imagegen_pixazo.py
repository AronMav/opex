"""Pixazo ImageGen provider — FLUX and SD via Pixazo gateway."""

import asyncio

import httpx

from helpers import validate_url_ssrf
from providers.base import ImageGenProvider, resolve_request_timeout


class PixazoImageGen(ImageGenProvider):
    name = "Pixazo"

    MODEL_ENDPOINTS = {
        "flux-schnell": "/flux-1-schnell/v1/get-image-batch",
        "flux-dev": "/flux-dev/v1/dev/textToImage",
        "flux-2-dev": "/generateT2I",
        "sd3": "/sd3/v1/getData",
        "sdxl": "/getImage/v1/getSDXLImage",
    }

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://gateway.pixazo.ai").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "flux-schnell"
        self.options = options or {}
        self._request_timeout = resolve_request_timeout(self.options)

    async def generate(self, http: httpx.AsyncClient, prompt: str,
                       size: str = "1024x1024", model: str | None = None,
                       quality: str = "standard") -> bytes:
        model_name = model or self.model
        endpoint_path = self.MODEL_ENDPOINTS.get(model_name)
        if not endpoint_path:
            endpoint_path = f"/{model_name}/v1/generate"

        size_presets = {
            "1024x1024": "square_hd",
            "512x512": "square",
            "768x1024": "portrait_4_3",
            "576x1024": "portrait_16_9",
            "1024x768": "landscape_4_3",
            "1024x576": "landscape_16_9",
        }

        if model_name in ("flux-2-dev",):
            width, height = map(int, size.split("x"))
            payload: dict = {
                "prompt": prompt,
                "width": width,
                "height": height,
                "steps": 8 if quality == "high" else 4,
            }
        else:
            image_size = size_presets.get(size, "square_hd")
            payload = {
                "prompt": prompt,
                "image_size": image_size,
                "num_images": 1,
                "output_format": "jpeg",
            }
            if quality == "high":
                payload["num_inference_steps"] = 25

        headers = {
            "Content-Type": "application/json",
            "Ocp-Apim-Subscription-Key": self.api_key,
        }

        resp = await http.post(
            f"{self.base_url}{endpoint_path}",
            json=payload,
            headers=headers,
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()

        if "request_id" in data:
            request_id = data["request_id"]
            for _ in range(60):
                await asyncio.sleep(1)
                poll_resp = await http.post(
                    f"{self.base_url}/flux-dev-polling/dev/getFluxDevStatus",
                    json={"requestId": request_id},
                    headers=headers,
                    timeout=self._request_timeout,
                )
                poll_resp.raise_for_status()
                poll_data = poll_resp.json()
                images = poll_data.get("images", [])
                if images:
                    # F052: SSRF-validate the provider-returned URL before
                    # fetching (matches fal/Runware) — the shared client has no
                    # SSRF resolver, so a reflected internal URL would otherwise
                    # reach loopback/metadata.
                    validate_url_ssrf(images[0]["url"])
                    img_resp = await http.get(images[0]["url"], timeout=self._request_timeout)
                    img_resp.raise_for_status()
                    return img_resp.content
            raise Exception("Pixazo generation timed out after 60s")

        image_url = data.get("output") or data.get("image_url")
        if image_url:
            validate_url_ssrf(image_url)  # F052: SSRF-validate before fetch
            img_resp = await http.get(image_url, timeout=self._request_timeout)
            img_resp.raise_for_status()
            return img_resp.content

        raise Exception(f"Unexpected Pixazo response: {data}")
