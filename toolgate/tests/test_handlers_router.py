import textwrap

from fastapi import FastAPI
from fastapi.testclient import TestClient

from handlers.loader import HandlerRegistry
from handlers.router import router as handlers_router

GOOD = textwrap.dedent('''\
    # <handler>
    #   <id>echo</id>
    #   <label lang="ru">Эхо</label>
    #   <label lang="en">Echo</label>
    #   <icon>file</icon>
    #   <match><mime>text/*</mime><max_size_mb>1</max_size_mb></match>
    #   <execution>sync</execution>
    #   <output>text</output>
    #   <order>5</order>
    #   <enabled>true</enabled>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text(file.bytes.decode("utf-8"))
''')

# capability-bearing handler exercises the R5 provider-fill on GET /handlers
WITH_CAP = textwrap.dedent('''\
    # <handler>
    #   <id>cap</id>
    #   <label lang="en">Cap</label>
    #   <match><mime>audio/*</mime></match>
    #   <capability>stt</capability>
    #   <execution>sync</execution>
    #   <output>text</output>
    #   <order>6</order>
    #   <enabled>true</enabled>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("cap")
''')

# an async handler exercises the 501-until-Phase-5 branch
ASYNC_H = textwrap.dedent('''\
    # <handler>
    #   <id>slow</id>
    #   <label lang="en">Slow</label>
    #   <match><mime>video/*</mime></match>
    #   <execution>async</execution>
    #   <output>text</output>
    #   <order>7</order>
    #   <enabled>true</enabled>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("slow")
''')


class _FakeProvider:
    name = "fake-stt-provider"


class _FakeRegistry:
    def __init__(self, active=None):
        self._active = active or {}
    async def aget_active(self, capability):
        return self._active.get(capability)


def _build_client(tmp_path, provider_active=None):
    import httpx

    builtin = tmp_path / "builtin"
    builtin.mkdir()
    (builtin / "echo.py").write_text(GOOD, encoding="utf-8")
    (builtin / "cap.py").write_text(WITH_CAP, encoding="utf-8")
    (builtin / "slow.py").write_text(ASYNC_H, encoding="utf-8")
    hreg = HandlerRegistry()
    hreg.load_all(str(builtin), None)

    app = FastAPI()
    app.include_router(handlers_router)
    # R12: handlers receive raw bytes; the router never fetches a URL. The
    # shared client is only used by provider calls (none in these tests).
    app.state.http_client = httpx.AsyncClient(
        transport=httpx.MockTransport(lambda r: httpx.Response(200))
    )
    app.state.registry = _FakeRegistry(provider_active)
    app.state.handlers = hreg
    return TestClient(app)


def _run(client, handler_id, *, content=b"hello-file", mime="text/plain",
         filename="a.txt", params="{}", language="ru", source_url=None):
    files = {"file": (filename, content, mime)}
    data = {"mime": mime, "filename": filename, "params": params,
            "language": language}
    if source_url is not None:
        data["source_url"] = source_url
    return client.post(f"/handlers/{handler_id}/run", files=files, data=data)


def test_get_handlers_shape_and_etag(tmp_path):
    client = _build_client(tmp_path)
    r = client.get("/handlers")
    assert r.status_code == 200
    body = r.json()
    ids = {h["id"] for h in body["handlers"]}
    assert {"echo", "cap", "slow"} <= ids
    assert "etag" in body and r.headers["etag"] == body["etag"]


def test_get_handlers_fills_provider_from_active(tmp_path):
    client = _build_client(tmp_path, provider_active={"stt": _FakeProvider()})
    body = client.get("/handlers").json()
    cap = next(h for h in body["handlers"] if h["id"] == "cap")
    assert cap["provider"] == "fake-stt-provider"
    echo = next(h for h in body["handlers"] if h["id"] == "echo")
    assert echo["provider"] is None  # no capability -> stays None


def test_get_handlers_provider_none_when_no_active(tmp_path):
    client = _build_client(tmp_path, provider_active={})
    body = client.get("/handlers").json()
    cap = next(h for h in body["handlers"] if h["id"] == "cap")
    assert cap["provider"] is None


def test_get_handlers_304_on_if_none_match(tmp_path):
    client = _build_client(tmp_path)
    etag = client.get("/handlers").headers["etag"]
    r = client.get("/handlers", headers={"If-None-Match": etag})
    assert r.status_code == 304




def test_run_sync_multipart_returns_scenario_outcome(tmp_path):
    client = _build_client(tmp_path)
    r = _run(client, "echo", content=b"hello-file")
    assert r.status_code == 200
    out = r.json()
    assert out == {
        "status": "ok",
        "summary_text": "hello-file",
        "artifact_urls": [],
        "reason": None,
    }


def test_run_missing_handler_404(tmp_path):
    client = _build_client(tmp_path)
    r = _run(client, "nope")
    assert r.status_code == 404


def test_run_async_handler_returns_202_and_accepted(tmp_path, monkeypatch):
    """Phase 5: async handler returns 202 Accepted + spawns runner out-of-process."""
    import asyncio
    import handlers.router as rmod

    async def _fake_exec(*args, **kwargs):
        # F026/F095: run_handler writes the spec to the runner's stdin.
        class _Stdin:
            def write(self, b): pass
            async def drain(self): pass
            def close(self): pass

        class _Proc:
            pid = 9999
            stdin = _Stdin()
        return _Proc()

    monkeypatch.setattr(rmod.asyncio, "create_subprocess_exec", _fake_exec)

    client = _build_client(tmp_path)
    r = _run(client, "slow", content=b"vid", mime="video/mp4", filename="v.mp4")
    assert r.status_code == 202
    assert r.json()["accepted"] is True


def test_run_sync_timeout_returns_timeout_outcome(tmp_path, monkeypatch):
    # Force the configured sync timeout to ~0 so any await trips it.
    import handlers.router as rmod
    monkeypatch.setattr(rmod, "HANDLER_SYNC_TIMEOUT_SECS", 0.0)
    client = _build_client(tmp_path)
    r = _run(client, "echo", content=b"hello-file")
    assert r.status_code == 200
    out = r.json()
    assert out["status"] == "timeout"
    assert out["reason"] == "per-execution timeout"
