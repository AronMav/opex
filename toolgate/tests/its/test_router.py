# toolgate/tests/its/test_router.py
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

def make_client(flows):
    app = FastAPI()
    app.include_router(itsr.router)
    async def fake_build(http): return flows
    itsr.build_flows = fake_build
    async def fake_creds(http): return {"login": "u", "password": "p"}
    itsr.get_credentials = fake_creds
    app.state.http_client = object()
    return TestClient(app)

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
