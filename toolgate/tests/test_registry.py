"""Unit tests for ProviderRegistry degraded-mode flag."""

import importlib

import httpx
import pytest
from registry import ProviderRegistry
from config import ProvidersConfig

from fastapi.testclient import TestClient


# ── helpers ───────────────────────────────────────────────────────────────────

# Empty providers payload — aload() parses it to zero providers, so the
# registry reports degraded. Patching httpx (the path aload actually uses)
# instead of the unused `_aload_config_from_api` symbol makes these tests
# genuinely exercise the load path rather than passing on connection-refused.
_EMPTY_PAYLOAD = {"json": {"version": 1, "active": {}, "providers": {}}}


def _patch_empty_refresh(monkeypatch):
    """Force the registry's real load path (`_refresh`) to yield zero providers,
    so the app starts degraded without a live Core fetch — and WITHOUT patching
    the process-global `httpx.AsyncClient` (which the app itself uses at startup,
    so a global fake breaks app import). Unlike the old `_aload_config_from_api`
    stub, `_refresh` IS on the `aload()` code path, so the patch actually governs
    the outcome instead of the test silently passing on connection-refused."""
    async def _empty_refresh(self):
        with self._lock:
            self.config = ProvidersConfig()
            self._instantiate_all()
    monkeypatch.setattr("registry.ProviderRegistry._refresh", _empty_refresh)


def _degraded_test_client(monkeypatch) -> TestClient:
    """Reload app with an empty provider config and return a TestClient over it."""
    _patch_empty_refresh(monkeypatch)
    import app as app_module
    importlib.reload(app_module)
    return TestClient(app_module.app)


def _install_fake_httpx(monkeypatch, *, payloads):
    """Replace httpx.AsyncClient with a fake that returns one payload per call.

    `payloads` is a list of dicts; each call pops from the front. After the
    list is exhausted, the last payload is reused (mirroring sticky test
    fixtures). Returns the shared `call_count` dict so callers can assert.
    """
    state = {"calls": 0}
    idx = {"i": 0}

    async def _get(url, headers=None, timeout=None):
        state["calls"] += 1
        i = min(idx["i"], len(payloads) - 1)
        idx["i"] += 1
        spec = payloads[i]
        return httpx.Response(
            spec.get("status", 200),
            json=spec.get("json"),
            headers=spec.get("headers", {}),
        )

    class _FakeClient:
        async def __aenter__(self): return self
        async def __aexit__(self, *a): return None
        async def get(self, *a, **kw): return await _get(*a, **kw)

    # Accept (and ignore) constructor args like `timeout=` — patching
    # httpx.AsyncClient is process-global, so other modules importing httpx
    # during an app reload (e.g. workspace_helpers) construct the fake too.
    monkeypatch.setattr("registry.httpx.AsyncClient", lambda *a, **kw: _FakeClient())
    return state


# ── tests ─────────────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_is_degraded_true_when_no_providers_loaded(monkeypatch):
    _install_fake_httpx(monkeypatch, payloads=[_EMPTY_PAYLOAD])
    reg = ProviderRegistry()
    await reg.aload()
    assert reg.is_degraded() is True


@pytest.mark.asyncio
async def test_is_degraded_false_after_successful_load(monkeypatch):
    _install_fake_httpx(monkeypatch, payloads=[{
        "json": {
            "version": 1,
            "active": {"stt": "local-whisper"},
            "providers": {
                "local-whisper": {
                    "type": "stt",
                    "driver": "whisper-local",
                    "base_url": "http://localhost:8300/v1",
                    "model": "faster-whisper-large-v3",
                    "enabled": True,
                },
            },
        },
        "headers": {"ETag": '"v1"'},
    }])
    reg = ProviderRegistry()
    await reg.aload()
    assert reg.is_degraded() is False


@pytest.mark.asyncio
async def test_health_reports_degraded_and_capabilities(monkeypatch):
    """/health must expose degraded flag + per-capability boolean map."""
    with _degraded_test_client(monkeypatch) as client:
        resp = client.get("/health")
    assert resp.status_code == 200
    body = resp.json()
    assert body["degraded"] is True
    assert body["loaded_providers"] == 0
    assert set(body["capabilities"].keys()) == {"stt", "tts", "vision", "imagegen", "embedding", "websearch"}
    assert all(v is False for v in body["capabilities"].values())


@pytest.mark.asyncio
async def test_stt_endpoint_returns_structured_503_when_degraded(monkeypatch):
    """When no STT provider is active, /transcribe-url returns structured 503.
    Exercises require_provider() — which all capability endpoints route through."""
    with _degraded_test_client(monkeypatch) as client:
        resp = client.post("/transcribe-url", json={"audio_url": "http://example/x.mp3"})
    assert resp.status_code == 503
    body = resp.json()
    assert body["error"] == "no_stt_provider"
    assert body["degraded"] is True
    assert "core" in body["hint"].lower() or "recover" in body["hint"].lower()


@pytest.mark.asyncio
async def test_tts_endpoint_also_uses_structured_503(monkeypatch):
    """Verify the shared dependency produces correct capability-scoped error for TTS too."""
    with _degraded_test_client(monkeypatch) as client:
        resp = client.post("/v1/audio/speech", json={"input": "test"})
    assert resp.status_code == 503
    assert resp.json()["error"] == "no_tts_provider"


@pytest.mark.asyncio
async def test_embedding_endpoint_uses_structured_503(monkeypatch):
    """Embedding endpoint must also return structured 503 (was using inline error before fix)."""
    with _degraded_test_client(monkeypatch) as client:
        resp = client.post("/v1/embeddings", json={"input": "hello"})
    assert resp.status_code == 503
    body = resp.json()
    assert body["error"] == "no_embedding_provider"
    assert body["degraded"] is True


# ── T5: pull-on-call (TTL=0) semantics ────────────────────────────────────────

@pytest.mark.asyncio
async def test_aget_active_collapses_to_one_call_within_ttl(monkeypatch):
    """Task 18: TTL=30s collapses many aget_active calls to ONE HTTP request."""
    registry = ProviderRegistry()
    state = _install_fake_httpx(monkeypatch, payloads=[{
        "json": {
            "version": 1,
            "active": {"stt": "p1"},
            "providers": {
                "p1": {"type": "stt", "driver": "whisper-local", "enabled": True},
            },
        },
        "headers": {"ETag": '"v1"'},
    }])

    for _ in range(5):
        await registry.aget_active("stt")
    assert state["calls"] == 1


@pytest.mark.asyncio
async def test_aget_active_falls_back_on_core_down(monkeypatch):
    """Core unreachable → aget_active returns last-known instance."""
    import time

    registry = ProviderRegistry()
    populated_payload = {
        "json": {
            "version": 1,
            "active": {"stt": "p1"},
            "providers": {
                "p1": {"type": "stt", "driver": "whisper-local", "enabled": True},
            },
        },
        "headers": {"ETag": '"v1"'},
    }

    # First call: populate from a healthy Core.
    _install_fake_httpx(monkeypatch, payloads=[populated_payload])
    first = await registry.aget_active("stt")
    assert first is not None

    # Now simulate Core down: any get() raises. Expire TTL so _refresh runs.
    class _BoomClient:
        async def __aenter__(self): return self
        async def __aexit__(self, *a): return None
        async def get(self, *a, **kw):
            raise httpx.ConnectError("simulated outage")

    monkeypatch.setattr("registry.httpx.AsyncClient", lambda: _BoomClient())
    registry._last_fetch = time.monotonic() - 31

    result = await registry.aget_active("stt")
    assert result is not None, "should return last-known instance after Core blip"


@pytest.mark.asyncio
async def test_provider_swap_takes_effect_after_ttl(monkeypatch):
    """Change Core's config → next aget_active after TTL expiry reflects it."""
    import time

    registry = ProviderRegistry()
    v1 = {
        "json": {
            "version": 1,
            "active": {"stt": "p1"},
            "providers": {
                "p1": {"type": "stt", "driver": "whisper-local", "enabled": True},
            },
        },
        "headers": {"ETag": '"v1"'},
    }
    v2 = {
        "json": {
            "version": 2,
            "active": {"stt": "p2"},
            "providers": {
                "p2": {"type": "stt", "driver": "openai", "enabled": True},
            },
        },
        "headers": {"ETag": '"v2"'},
    }

    _install_fake_httpx(monkeypatch, payloads=[v1, v2])
    first = await registry.aget_active("stt")
    # Expire TTL so the next call re-fetches and sees v2.
    registry._last_fetch = time.monotonic() - 31
    second = await registry.aget_active("stt")
    assert type(first) is not type(second), "different drivers expected"


def test_no_reload_endpoint(monkeypatch):
    """POST /reload should return 404 or 405 — endpoint removed."""
    # Start the app degraded without a real Core fetch (see _patch_empty_refresh).
    _patch_empty_refresh(monkeypatch)
    import app as app_module
    importlib.reload(app_module)
    with TestClient(app_module.app) as client:
        resp = client.post("/reload")
        assert resp.status_code in (404, 405)


# ── Task 18: TTL=30s + ETag conditional GET ───────────────────────────────────

import time

import httpx as _httpx


@pytest.mark.asyncio
async def test_refresh_uses_ttl_cache(monkeypatch):
    """Два вызова _refresh в пределах 30s должны делать ОДИН HTTP-запрос."""
    registry = ProviderRegistry()
    call_count = {"n": 0}

    async def fake_get(url, headers=None, timeout=None):
        call_count["n"] += 1
        return _httpx.Response(
            200,
            json={"version": 1, "active": {"embedding": "x"},
                  "providers": {"x": {"type": "embedding", "driver": "openai"}}},
            headers={"ETag": '"abc"'},
        )

    class FakeClient:
        async def __aenter__(self): return self
        async def __aexit__(self, *a): return None
        async def get(self, *a, **kw): return await fake_get(*a, **kw)

    monkeypatch.setattr(_httpx, "AsyncClient", lambda: FakeClient())

    await registry._refresh()
    await registry._refresh()
    assert call_count["n"] == 1


@pytest.mark.asyncio
async def test_refresh_sends_if_none_match_after_first_call(monkeypatch):
    registry = ProviderRegistry()
    seen_headers = []

    async def fake_get(url, headers=None, timeout=None):
        seen_headers.append(headers or {})
        if "If-None-Match" in (headers or {}):
            return _httpx.Response(304)
        return _httpx.Response(
            200,
            json={"version": 1, "active": {}, "providers": {}},
            headers={"ETag": '"abc"'},
        )

    class FakeClient:
        async def __aenter__(self): return self
        async def __aexit__(self, *a): return None
        async def get(self, *a, **kw): return await fake_get(*a, **kw)

    monkeypatch.setattr(_httpx, "AsyncClient", lambda: FakeClient())

    await registry._refresh()
    # Заставим истечь TTL
    registry._last_fetch = time.monotonic() - 31
    await registry._refresh()
    assert seen_headers[1].get("If-None-Match") == '"abc"'


def test_silero_driver_registered():
    """The silero TTS driver must resolve to SileroTTS in the driver map."""
    from registry import get_driver_map
    from providers.tts_silero import SileroTTS
    assert get_driver_map()[("tts", "silero")] is SileroTTS
