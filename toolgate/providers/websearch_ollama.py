"""Ollama Cloud web search provider."""
import logging
from providers.base import resolve_request_timeout
log = logging.getLogger(__name__)

class OllamaWebSearch:
    name = "Ollama Web Search"
    def __init__(self, base_url, api_key=None, model=None, options=None):
        self.base_url = (base_url or "https://ollama.com").rstrip("/")
        self.api_key = api_key or ""
        self._timeout = resolve_request_timeout(options or {}, 30.0)
    async def search(self, http, query, max_results=5):
        headers = {"Authorization": f"Bearer {self.api_key}"} if self.api_key else {}
        resp = await http.post(
            f"{self.base_url}/api/web_search",
            json={"query": query, "max_results": max_results},
            headers=headers, timeout=self._timeout,
        )
        resp.raise_for_status()
        return [
            {"title": r.get("title", ""), "url": r.get("url", ""), "content": r.get("content", "")}
            for r in resp.json().get("results", [])
        ]
