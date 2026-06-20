"""Brave Search API provider."""
import logging
from providers.base import resolve_request_timeout
log = logging.getLogger(__name__)

class BraveWebSearch:
    name = "Brave Search"
    def __init__(self, base_url, api_key=None, model=None, options=None):
        self.base_url = (base_url or "https://api.search.brave.com").rstrip("/")
        self.api_key = api_key or ""
        self._timeout = resolve_request_timeout(options or {}, 30.0)
    async def search(self, http, query, max_results=5):
        headers = {"X-Subscription-Token": self.api_key, "Accept": "application/json"}
        resp = await http.get(
            f"{self.base_url}/res/v1/web/search",
            params={"q": query, "count": max_results},
            headers=headers, timeout=self._timeout,
        )
        resp.raise_for_status()
        return [
            {"title": r.get("title", ""), "url": r.get("url", ""), "content": r.get("description", "")}
            for r in resp.json().get("web", {}).get("results", [])
        ]
