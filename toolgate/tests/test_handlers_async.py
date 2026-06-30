"""Async handler path (R12): the Phase-2 run_handler returns 202 + spawns the
runner out-of-process from a tempfile PATH (no loopback fetch); the runner posts
progress + complete callbacks to core, reading bytes from the temp path."""
import json
import sys
from pathlib import Path

import httpx
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from handlers.loader import HandlerRegistry  # noqa: E402
from handlers import runner as runner_mod  # noqa: E402


class _UploadFile:
    """Minimal stand-in for fastapi.UploadFile."""
    def __init__(self, data: bytes):
        self._data = data

    async def read(self) -> bytes:
        return self._data


class _FakeRequest:
    """Minimal stand-in for fastapi.Request exposing app.state.handlers."""
    def __init__(self, registry):
        self.app = type("A", (), {"state": type("S", (), {"handlers": registry})()})()


@pytest.mark.asyncio
async def test_async_handler_run_returns_202_and_spawns_runner_with_tempfile(monkeypatch, tmp_path):
    """An execution=async handler must NOT run inline — run_handler writes the
    upload bytes to a tempfile, returns 202, and spawns the runner with the PATH."""
    from handlers import router as router_mod

    reg = HandlerRegistry()
    reg.load_all(
        builtin_dir=str(Path(__file__).resolve().parents[1] / "handlers" / "builtin"),
        workspace_dir=None,
    )
    # summarize_video is the only async builtin (Task 5).
    assert reg.get("summarize_video") is not None
    assert reg.get("summarize_video").descriptor.execution == "async"

    spawned = {}

    async def fake_exec(*args, **kwargs):
        spawned["argv"] = args

        class _Proc:
            pid = 4242
        return _Proc()

    monkeypatch.setattr(router_mod.asyncio, "create_subprocess_exec", fake_exec)

    resp = await router_mod.run_handler(
        "summarize_video",
        _FakeRequest(reg),
        file=_UploadFile(b"VIDEOBYTES"),
        mime="video/mp4",
        filename="v.mp4",
        params="{}",
        language="ru",
        job_id="job-123",
        source_url=None,
    )
    assert resp.status_code == 202
    payload = json.loads(bytes(resp.body))
    assert payload == {"accepted": True, "job_id": "job-123"}

    argv = " ".join(str(a) for a in spawned["argv"])
    assert "runner" in argv
    # The spawned spec must reference a real temp path holding the bytes (NOT a URL).
    spec_arg = spawned["argv"][-1]
    spec = json.loads(spec_arg)
    assert spec["job_id"] == "job-123"
    assert spec["temp_path"]
    assert Path(spec["temp_path"]).read_bytes() == b"VIDEOBYTES"
    Path(spec["temp_path"]).unlink(missing_ok=True)


@pytest.mark.asyncio
async def test_runner_reads_tempfile_then_posts_progress_and_complete(monkeypatch, tmp_path):
    """The runner reads bytes from the temp path (NO network fetch), runs the
    handler, posts progress + a final ScenarioOutcome (4-key wire), and deletes
    the temp file afterwards."""
    posts = []

    class FakeAsyncClient:
        def __init__(self, *a, **k):
            pass
        async def __aenter__(self):
            return self
        async def __aexit__(self, *a):
            return False
        async def post(self, url, json=None, headers=None, **k):
            posts.append((url, json))
            return httpx.Response(200, request=httpx.Request("POST", url))
        async def aclose(self):
            pass

    monkeypatch.setattr(runner_mod.httpx, "AsyncClient", FakeAsyncClient)

    async def fake_run(ctx, file, params):
        await ctx.progress("digest", 50)
        return ctx.result.text("итоговый конспект")

    class FakeLoaded:
        class descriptor:
            execution = "async"
        run = staticmethod(fake_run)

    class FakeReg:
        def load_all(self, **k):
            pass
        def get(self, _id):
            return FakeLoaded()

    class FakeResultBuilder:
        def text(self, s):
            class _R:
                def to_dict(self_inner):
                    return {"status": "ok", "summary_text": s,
                            "artifact_urls": [], "reason": None}
            return _R()

    class FakeCtx:
        def __init__(self):
            self.result = FakeResultBuilder()
        async def progress(self, phase, pct):
            pass

    monkeypatch.setattr(runner_mod, "_load_registry", lambda http: FakeReg())
    monkeypatch.setattr(runner_mod, "build_context", lambda *a, **k: FakeCtx())

    temp = tmp_path / "upload.bin"
    temp.write_bytes(b"FAKEBYTES")

    spec = {
        "handler_id": "summarize_video",
        "temp_path": str(temp),
        "source_url": None,
        "mime": "video/mp4",
        "filename": "v.mp4",
        "params": {},
        "language": "ru",
        "job_id": "job-123",
        "core_url": "http://127.0.0.1:18789",
        "auth_token": "tok",
    }
    await runner_mod.run_job(spec)

    urls = [u for u, _ in posts]
    assert any(u.endswith("/api/files/jobs/job-123/progress") for u in urls), urls
    assert any(u.endswith("/api/files/jobs/job-123/complete") for u in urls), urls
    complete = next(b for u, b in posts if u.endswith("/complete"))
    assert complete == {"status": "ok", "summary_text": "итоговый конспект",
                        "artifact_urls": [], "reason": None}
    # Temp file deleted by the runner's finally.
    assert not temp.exists(), "runner must delete the temp file"


@pytest.mark.asyncio
async def test_runner_sends_x_job_token_header_when_spec_has_callback_token(monkeypatch, tmp_path):
    """FIX 5: when the job spec contains 'callback_token', the runner must
    forward it as the 'X-Job-Token' header on ALL POST requests (progress +
    complete). When absent, no X-Job-Token header is sent."""
    posts: list[tuple[str, dict | None, dict | None]] = []

    class FakeAsyncClient:
        def __init__(self, *a, **k):
            pass
        async def __aenter__(self):
            return self
        async def __aexit__(self, *a):
            return False
        async def post(self, url, json=None, headers=None, **k):
            posts.append((url, json, dict(headers) if headers else {}))
            return httpx.Response(200, request=httpx.Request("POST", url))
        async def aclose(self):
            pass

    monkeypatch.setattr(runner_mod.httpx, "AsyncClient", FakeAsyncClient)

    class FakeLoaded:
        class descriptor:
            execution = "async"

        @staticmethod
        async def run(ctx, file, params):
            await ctx.progress("digest", 50)

            class _R:
                def to_dict(self):
                    return {"status": "ok", "summary_text": "done",
                            "artifact_urls": [], "reason": None}
            return _R()

    class FakeReg:
        def load_all(self, **k): pass
        def get(self, _id): return FakeLoaded()

    class FakeCtx:
        async def progress(self, phase, pct):
            pass

    monkeypatch.setattr(runner_mod, "_load_registry", lambda http: FakeReg())
    monkeypatch.setattr(runner_mod, "build_context", lambda *a, **k: FakeCtx())

    temp = tmp_path / "upload.bin"
    temp.write_bytes(b"DATA")

    # ── Case 1: callback_token present — must appear in X-Job-Token header ─────
    spec_with_token = {
        "handler_id": "summarize_video",
        "temp_path": str(temp),
        "source_url": None,
        "mime": "video/mp4",
        "filename": "v.mp4",
        "params": {},
        "language": "ru",
        "job_id": "job-abc",
        "core_url": "http://127.0.0.1:18789",
        "auth_token": "tok",
        "callback_token": "12345.deadbeef",
    }
    temp.write_bytes(b"DATA")
    await runner_mod.run_job(spec_with_token)

    for url, _body, hdrs in posts:
        assert hdrs.get("X-Job-Token") == "12345.deadbeef", (
            f"X-Job-Token missing on POST to {url}: headers={hdrs}"
        )

    # ── Case 2: callback_token absent — no X-Job-Token header sent ────────────
    posts.clear()
    spec_no_token = {**spec_with_token}
    del spec_no_token["callback_token"]
    temp.write_bytes(b"DATA")
    await runner_mod.run_job(spec_no_token)

    for url, _body, hdrs in posts:
        assert "X-Job-Token" not in hdrs, (
            f"Unexpected X-Job-Token on POST to {url} when spec has no callback_token"
        )
