"""SearXNG web search provider (requires JSON format enabled on the instance)."""
import httpx, logging
from providers.base import resolve_request_timeout
log = logging.getLogger(__name__)

class SearxngWebSearch:
    name = "SearXNG"
    def __init__(self, base_url, api_key=None, model=None, options=None):
        self.base_url = (base_url or "http://localhost:8080").rstrip("/")
        self._timeout = resolve_request_timeout(options or {}, 30.0)
    async def search(self, http, query, max_results=5):
        resp = await http.get(
            f"{self.base_url}/search",
            params={"q": query, "format": "json"},
            timeout=self._timeout,
        )
        resp.raise_for_status()
        results = resp.json().get("results", [])[:max_results]
        return [
            {"title": r.get("title", ""), "url": r.get("url", ""), "content": r.get("content", "")}
            for r in results
        ]
