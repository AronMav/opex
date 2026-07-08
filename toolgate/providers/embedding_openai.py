"""OpenAI-compatible embedding provider (works with OpenRouter, OpenAI, Azure, etc.)."""
import httpx
import logging

from providers.base import resolve_request_timeout, join_openai_path

log = logging.getLogger(__name__)


class OpenAIEmbedding:
    name = "OpenAI Embedding"

    def __init__(
        self,
        base_url: str,
        api_key: str | None = None,
        model: str | None = None,
        options: dict | None = None,
    ):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "text-embedding-3-small"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def embed(
        self,
        http: httpx.AsyncClient,
        texts: list[str],
        model: str | None = None,
    ) -> list[list[float]]:
        url = join_openai_path(self.base_url, "/v1/embeddings")
        headers = {"Content-Type": "application/json"}
        if self.api_key:
            headers["Authorization"] = f"Bearer {self.api_key}"

        body = {
            "model": model or self.model,
            "input": texts,
        }
        resp = await http.post(url, json=body, headers=headers, timeout=self._request_timeout if self._request_timeout is not None else 60.0)
        resp.raise_for_status()
        data = resp.json()

        # OpenAI format: {"data": [{"embedding": [...], "index": 0}, ...]}
        embeddings = sorted(data["data"], key=lambda x: x["index"])
        return [item["embedding"] for item in embeddings]
