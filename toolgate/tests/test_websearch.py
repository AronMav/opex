import pytest, httpx
from providers.websearch_ollama import OllamaWebSearch
from providers.websearch_brave import BraveWebSearch
from providers.websearch_searxng import SearxngWebSearch

@pytest.mark.asyncio
async def test_ollama_normalizes():
    def handler(req):
        return httpx.Response(200, json={"results": [{"title": "T", "url": "U", "content": "C"}]})
    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    out = await OllamaWebSearch("https://ollama.com", api_key="k").search(http, "q", 5)
    assert out == [{"title": "T", "url": "U", "content": "C"}]

@pytest.mark.asyncio
async def test_brave_maps_description_to_content():
    def handler(req):
        return httpx.Response(200, json={"web": {"results": [{"title": "T", "url": "U", "description": "D"}]}})
    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    out = await BraveWebSearch("https://api.search.brave.com", api_key="k").search(http, "q", 3)
    assert out == [{"title": "T", "url": "U", "content": "D"}]

@pytest.mark.asyncio
async def test_searxng_slices_to_max_results():
    def handler(req):
        return httpx.Response(200, json={"results": [{"title": f"T{i}", "url": f"U{i}", "content": f"C{i}"} for i in range(10)]})
    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    out = await SearxngWebSearch("http://localhost:8080").search(http, "q", 2)
    assert len(out) == 2 and out[0]["title"] == "T0"


def _app_with_registry(reg):
    from fastapi import FastAPI
    from routers.search import router
    app = FastAPI()
    app.include_router(router)
    app.state.registry = reg
    app.state.http_client = None  # plugins are faked, so http is unused
    return app

def test_router_503_when_no_active_provider():
    from fastapi.testclient import TestClient
    class Reg:
        async def aget_active(self, cap): return None
        async def aget_instance(self, name): return None
    r = TestClient(_app_with_registry(Reg())).post("/v1/search", json={"query": "q"})
    assert r.status_code == 503 and r.json()["error"] == "no_websearch_provider"

def test_router_bad_max_results_falls_back_to_default():
    from fastapi.testclient import TestClient
    class Prov:
        async def search(self, http, query, max_results): return []
    class Reg:
        async def aget_active(self, cap): return Prov()
        async def aget_instance(self, name): return Prov()
    r = TestClient(_app_with_registry(Reg())).post("/v1/search", json={"query": "q", "max_results": "abc"})
    assert r.status_code == 200

def test_router_body_provider_uses_instance_override():
    from fastapi.testclient import TestClient
    class Prov:
        async def search(self, http, query, max_results): return [{"title": "T", "url": "U", "content": "C"}]
    calls = {}
    class Reg:
        async def aget_active(self, cap): calls["active"] = cap; return Prov()
        async def aget_instance(self, name): calls["instance"] = name; return Prov()
    r = TestClient(_app_with_registry(Reg())).post("/v1/search", json={"query": "q", "provider": "ws-brave"})
    assert r.status_code == 200 and r.json()["results"][0]["title"] == "T"
    assert calls.get("instance") == "ws-brave" and "active" not in calls  # override path, not active
