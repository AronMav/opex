"""Stability AI ImageGen provider — SD3, SD3.5 image generation."""

import httpx

from providers.base import ImageGenProvider, resolve_request_timeout


class StabilityImageGen(ImageGenProvider):
    name = "Stability AI"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.stability.ai").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "sd3-medium"
        opts = options or {}
        self.seed = opts.get("seed")
        self.negative_prompt = opts.get("negative_prompt")
        self._request_timeout = resolve_request_timeout(opts)

    async def generate(self, http: httpx.AsyncClient, prompt: str,
                       size: str = "1024x1024", model: str | None = None,
                       quality: str = "standard") -> bytes:
        model_name = model or self.model
        endpoint = f"{self.base_url}/v2beta/stable-image/generate/sd3"

        aspect = self._size_to_aspect(size)

        data: dict = {
            "prompt": prompt,
            "model": model_name,
            "aspect_ratio": aspect,
            "output_format": "png",
        }
        if self.seed is not None:
            data["seed"] = str(self.seed)
        if self.negative_prompt and "turbo" not in model_name:
            data["negative_prompt"] = self.negative_prompt

        resp = await http.post(
            endpoint,
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Accept": "image/*",
            },
            data=data,
            files={"none": ""},
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        return resp.content

    @staticmethod
    def _size_to_aspect(size: str) -> str:
        try:
            w, h = map(int, size.lower().split("x"))
        except Exception:
            return "1:1"
        ratio = w / h
        aspects = [
            (1.0, "1:1"), (16/9, "16:9"), (9/16, "9:16"),
            (21/9, "21:9"), (9/21, "9:21"), (3/2, "3:2"),
            (2/3, "2:3"), (4/5, "4:5"), (5/4, "5:4"),
        ]
        return min(aspects, key=lambda a: abs(a[0] - ratio))[1]
