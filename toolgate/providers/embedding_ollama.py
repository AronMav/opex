"""Ollama-compatible embedding provider."""
import httpx
import logging

from providers.base import resolve_request_timeout

log = logging.getLogger(__name__)


class OllamaEmbedding:
    name = "Ollama Embedding"

    def __init__(
        self,
        base_url: str,
        api_key: str | None = None,
        model: str | None = None,
        options: dict | None = None,
    ):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "qwen3-embedding:4b"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def embed(
        self,
        http: httpx.AsyncClient,
        texts: list[str],
        model: str | None = None,
    ) -> list[list[float]]:
        url = f"{self.base_url}/api/embeddings"
        headers = {}
        if self.api_key:
            headers["Authorization"] = f"Bearer {self.api_key}"

        # Ollama returns {"embedding": [...]} for a single prompt,
        # or an array if multiple prompts sent one by one.
        # Wrap single text or pass array.
        if len(texts) == 1:
            body = {"model": model or self.model, "prompt": texts[0]}
            resp = await http.post(url, json=body, headers=headers, timeout=self._request_timeout if self._request_timeout is not None else 60.0)
            resp.raise_for_status()
            data = resp.json()
            return [data["embedding"]]
        else:
            # Batch: send each separately and collect
            results = []
            for text in texts:
                body = {"model": model or self.model, "prompt": text}
                resp = await http.post(url, json=body, headers=headers, timeout=self._request_timeout if self._request_timeout is not None else 60.0)
                resp.raise_for_status()
                data = resp.json()
                results.append(data["embedding"])
            return results
