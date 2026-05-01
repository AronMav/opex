"""Replicate Vision provider — cloud vision via Replicate API."""

import asyncio
import base64

import httpx

from providers.base import resolve_request_timeout


class ReplicateVision:
    name = "Replicate"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.replicate.com/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "lucataco/moondream2:72ccb656353c348c1385df54b237eeb7bfa874bf11486cf0b9473e691b662d31"
        opts = options or {}
        self.temperature = opts.get("temperature", 0.7)
        self._request_timeout = resolve_request_timeout(opts)

    async def describe(self, http: httpx.AsyncClient, image_bytes: bytes,
                       content_type: str, prompt: str,
                       max_tokens: int = 2000) -> str:
        b64_data = base64.b64encode(image_bytes).decode("utf-8")
        data_uri = f"data:{content_type};base64,{b64_data}"

        model_parts = self.model.split(":")
        model_version = model_parts[1] if len(model_parts) > 1 else None

        if "moondream" in self.model:
            input_data = {"image": data_uri, "prompt": prompt}
        else:
            input_data = {
                "image": data_uri,
                "prompt": prompt,
                "max_tokens": max_tokens,
                "temperature": self.temperature,
            }

        create_resp = await http.post(
            f"{self.base_url}/predictions",
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
            json={"version": model_version, "input": input_data},
            timeout=self._request_timeout,
        )
        create_resp.raise_for_status()
        prediction_id = create_resp.json()["id"]

        for _ in range(100):
            poll_resp = await http.get(
                f"{self.base_url}/predictions/{prediction_id}",
                headers={"Authorization": f"Bearer {self.api_key}"},
                timeout=self._request_timeout,
            )
            poll_resp.raise_for_status()
            result = poll_resp.json()
            status = result["status"]

            if status == "succeeded":
                output = result.get("output", "")
                if isinstance(output, list):
                    return "".join(output)
                return str(output)
            elif status in ("failed", "canceled"):
                raise Exception(f"Replicate {status}: {result.get('error')}")

            await asyncio.sleep(0.3)

        raise Exception("Replicate prediction timed out after 30s")
