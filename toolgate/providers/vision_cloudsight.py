"""CloudSight Vision provider — image recognition API."""

import asyncio

import httpx

from providers.base import resolve_request_timeout


class CloudSightVision:
    name = "CloudSight"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.cloudsightapi.com").rstrip("/")
        self.api_key = api_key or ""
        opts = options or {}
        self.language = opts.get("language", "en")
        self._request_timeout = resolve_request_timeout(opts)

    async def describe(self, http: httpx.AsyncClient, image_bytes: bytes,
                       content_type: str, prompt: str,
                       max_tokens: int = 2000) -> str:
        files = {
            "image_request[image]": ("image.jpg", image_bytes, content_type),
        }
        data = {
            "image_request[locale]": self.language,
        }

        resp = await http.post(
            f"{self.base_url}/image_requests",
            headers={"Authorization": f"CloudSight {self.api_key}"},
            files=files,
            data=data,
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        result = resp.json()

        token = result.get("token")
        if not token:
            return result.get("name", "")

        for _ in range(30):
            await asyncio.sleep(1)
            poll_resp = await http.get(
                f"{self.base_url}/image_responses/{token}",
                headers={"Authorization": f"CloudSight {self.api_key}"},
                timeout=self._request_timeout,
            )
            poll_resp.raise_for_status()
            poll_result = poll_resp.json()
            status = poll_result.get("status")

            if status == "completed":
                return poll_result.get("name", "")
            elif status in ("skipped", "timeout"):
                raise Exception(f"CloudSight {status}: {poll_result.get('reason', 'unknown')}")

        raise Exception("CloudSight recognition timed out after 30s")
