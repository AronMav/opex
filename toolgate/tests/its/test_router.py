# toolgate/tests/its/test_router.py
import httpx
import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
import its.router as itsr
from its.flows import ItsBusy

class FakeFlows:
    def __init__(self, results=None, read=None, busy=False):
        self._results = results or []; self._read = read or {}; self._busy = busy
    async def ensure_logged_in(self, creds):
        if self._busy: raise ItsBusy("busy")
    async def search(self, query, db=None): return self._results
    async def read(self, ref): return self._read

def make_client(flows, counter=None):
    """flows: инстанс FakeFlows или фабрика (callable) для последовательных билдов."""
    app = FastAPI()
    app.include_router(itsr.router)
    async def fake_build(http):
        if counter is not None:
            counter["n"] += 1
        return flows() if callable(flows) else flows
    itsr.build_flows = fake_build
    itsr._flows = None            # сброс синглтона между тестами
    itsr._cache = itsr.TTLCache() # и кэша ответов
    async def fake_creds(http): return {"login": "u", "password": "p"}
    itsr.get_credentials = fake_creds
    app.state.http_client = object()
    return TestClient(app)

def _stale_error(status=404, body="Session abc not found"):
    req = httpx.Request("POST", "http://render/automation")
    resp = httpx.Response(status, text=body, request=req)
    return httpx.HTTPStatusError(f"{status}", request=req, response=resp)

def test_search_returns_results():
    c = make_client(FakeFlows(results=[{"title": "T", "ref": "r", "snippet": "s", "db": "v854doc"}]))
    r = c.post("/its/search", json={"query": "регламент"})
    assert r.status_code == 200
    assert r.json()["results"][0]["title"] == "T"

def test_read_returns_markdown():
    c = make_client(FakeFlows(read={"title": "T", "markdown": "# T", "url": "u", "images_omitted": 0}))
    r = c.post("/its/read", json={"ref": "db/v854doc#bookmark:adm:TI1"})
    assert r.status_code == 200
    assert r.json()["markdown"] == "# T"

def test_busy_returns_409():
    c = make_client(FakeFlows(busy=True))
    r = c.post("/its/read", json={"ref": "x"})
    assert r.status_code == 409
    assert r.json()["error"] == "its_busy"

def test_flows_singleton_reused_across_requests():
    # Регрессия: build_flows на каждый запрос = новая вечная вкладка в
    # persistent-профиле renderer'а → mem_limit контейнера → goto-таймауты.
    counter = {"n": 0}
    c = make_client(FakeFlows(results=[]), counter=counter)
    assert c.post("/its/search", json={"query": "a"}).status_code == 200
    assert c.post("/its/search", json={"query": "b"}).status_code == 200
    assert counter["n"] == 1

def test_stale_session_rebuilds_flows_and_retries():
    # Рестарт renderer'а/закрытая вкладка → 404 Session not found:
    # роутер должен пересоздать flows и повторить операцию один раз.
    class Stale(FakeFlows):
        async def search(self, query, db=None): raise _stale_error()
    good = FakeFlows(results=[{"title": "T", "ref": "r", "snippet": "s", "db": "d"}])
    instances = [Stale(), good]
    counter = {"n": 0}
    c = make_client(lambda: instances.pop(0), counter=counter)
    r = c.post("/its/search", json={"query": "x"})
    assert r.status_code == 200
    assert r.json()["results"][0]["title"] == "T"
    assert counter["n"] == 2

def test_closed_page_500_also_retries():
    class Stale(FakeFlows):
        async def search(self, query, db=None):
            raise _stale_error(500, "Target page, context or browser has been closed")
    good = FakeFlows(results=[{"title": "T2", "ref": "r", "snippet": "s", "db": "d"}])
    instances = [Stale(), good]
    c = make_client(lambda: instances.pop(0))
    r = c.post("/its/search", json={"query": "x"})
    assert r.status_code == 200
    assert r.json()["results"][0]["title"] == "T2"

def test_non_stale_error_is_not_retried():
    class Boom(FakeFlows):
        async def search(self, query, db=None): raise RuntimeError("boom")
    counter = {"n": 0}
    c = make_client(lambda: Boom(), counter=counter)
    r = c.post("/its/search", json={"query": "y"})
    assert r.status_code == 502
    assert counter["n"] == 1
