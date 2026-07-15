"""Task 10: profile-driven routing — X-Opex-Providers search chain,
X-Opex-Voice header, registry category fallback (no `provider_active` row).

Fixtures are constructed directly (grep of toolgate/tests turned up no shared
`make_registry`/`search_client_*` fixtures) mirroring the pattern used by
tests/test_registry.py: build a `ProviderRegistry` with `.config`/`._instances`
set directly, and skip the network `_refresh()` by pre-populating
`._last_fetch` + a non-empty `.config.providers` dict.
"""

import time

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from config import ProviderConfig, ProvidersConfig
from registry import ProviderRegistry
from routers import search as search_router_module
from routers import tts as tts_router_module


def _make_registry(providers: dict, active: dict) -> ProviderRegistry:
    """Build a ProviderRegistry with real driver instances (via _instantiate_all)
    and skip the network _refresh() for the duration of the test."""
    reg = ProviderRegistry()
    reg.config = ProvidersConfig(
        version=1,
        active=active,
        providers={pid: ProviderConfig(**cfg) for pid, cfg in providers.items()},
    )
    reg._instantiate_all()
    reg._last_fetch = time.monotonic()
    return reg


class _FakeSearchProvider:
    """Minimal WebSearchProvider stand-in — no HTTP, deterministic result/failure."""

    def __init__(self, name: str, *, raises: bool = False, results=None):
        self.name = name
        self._raises = raises
        self._results = results if results is not None else [{"title": name}]

    async def search(self, http, query, max_results=5):
        if self._raises:
            raise RuntimeError(f"{self.name} provider failed")
        return self._results


def _build_search_app(registry: ProviderRegistry) -> FastAPI:
    app = FastAPI()
    app.include_router(search_router_module.router)
    app.state.registry = registry
    app.state.http_client = None  # fake providers never touch it
    return app


class _FakeTTSProvider:
    """Minimal TTSProvider stand-in — records the `voice` arg it was called
    with instead of doing any real synthesis, so tests can assert on it."""

    def __init__(self, name: str = "fake-tts"):
        self.name = name
        self.model = None
        self.calls: list[dict] = []

    async def synthesize(self, http, text, voice, model=None, response_format="mp3", registry=None):
        self.calls.append({"text": text, "voice": voice, "model": model, "response_format": response_format})
        return b"fake-audio-bytes"


def _build_tts_app(registry: ProviderRegistry) -> FastAPI:
    app = FastAPI()
    app.include_router(tts_router_module.router)
    app.state.registry = registry
    app.state.http_client = None  # fake provider never touches it
    return app


def _skip_network(reg: ProviderRegistry) -> None:
    """Prevent aget_* from attempting a real HTTP call to Core during the test."""
    if not reg.config.providers:
        reg.config.providers = {
            "placeholder": ProviderConfig(type="unused", driver="none", enabled=False),
        }
    reg._last_fetch = time.monotonic()


# ── registry.aget_active category fallback ────────────────────────────────────

@pytest.mark.asyncio
async def test_aget_active_falls_back_to_category():
    reg = _make_registry(
        providers={
            "tts-b": {"type": "tts", "driver": "minimax", "enabled": True},
            "tts-a": {"type": "tts", "driver": "minimax", "enabled": True},
            "off": {"type": "tts", "driver": "minimax", "enabled": False},
        },
        active={},
    )
    p = await reg.aget_active("tts")
    assert p is not None
    assert p is reg._instances["tts-a"], "sorted-by-id fallback should pick tts-a first"


@pytest.mark.asyncio
async def test_aget_active_prefers_explicit_active_id():
    """Fallback must only engage when the active-id row is absent/uninstantiated."""
    reg = _make_registry(
        providers={
            "tts-a": {"type": "tts", "driver": "minimax", "enabled": True},
            "tts-b": {"type": "tts", "driver": "minimax", "enabled": True},
        },
        active={"tts": "tts-b"},
    )
    p = await reg.aget_active("tts")
    assert p is reg._instances["tts-b"]


@pytest.mark.asyncio
async def test_aget_active_returns_none_when_no_provider_of_category():
    reg = _make_registry(
        providers={"stt-a": {"type": "stt", "driver": "openai", "enabled": True}},
        active={},
    )
    assert await reg.aget_active("tts") is None


# ── /v1/search provider chain via X-Opex-Providers ────────────────────────────

def test_search_tries_provider_chain_in_header_order():
    reg = ProviderRegistry()
    _skip_network(reg)
    reg._instances["bad"] = _FakeSearchProvider("bad", raises=True)
    reg._instances["good"] = _FakeSearchProvider("good", results=[{"title": "ok"}])

    app = _build_search_app(reg)
    with TestClient(app) as client:
        resp = client.post(
            "/v1/search",
            json={"query": "q"},
            headers={"X-Opex-Providers": "bad,good"},
        )
    assert resp.status_code == 200
    assert resp.json()["results"] == [{"title": "ok"}]


def test_search_body_provider_overrides_header_chain():
    reg = ProviderRegistry()
    _skip_network(reg)
    reg._instances["bad"] = _FakeSearchProvider("bad", raises=True)
    reg._instances["good"] = _FakeSearchProvider("good", results=[{"title": "ok"}])
    reg._instances["explicit"] = _FakeSearchProvider("explicit", results=[{"title": "explicit"}])

    app = _build_search_app(reg)
    with TestClient(app) as client:
        resp = client.post(
            "/v1/search",
            json={"query": "q", "provider": "explicit"},
            headers={"X-Opex-Providers": "bad,good"},
        )
    assert resp.status_code == 200
    assert resp.json()["results"] == [{"title": "explicit"}]


def test_search_all_chain_providers_fail_returns_502():
    reg = ProviderRegistry()
    _skip_network(reg)
    reg._instances["bad"] = _FakeSearchProvider("bad", raises=True)
    reg._instances["also-bad"] = _FakeSearchProvider("also-bad", raises=True)

    app = _build_search_app(reg)
    with TestClient(app) as client:
        resp = client.post(
            "/v1/search",
            json={"query": "q"},
            headers={"X-Opex-Providers": "bad,also-bad"},
        )
    assert resp.status_code == 502


def test_search_no_header_falls_back_to_legacy_active():
    reg = ProviderRegistry()
    reg.config = ProvidersConfig(
        version=1,
        active={"websearch": "good"},
        providers={"good": ProviderConfig(type="websearch", driver="searxng", enabled=True)},
    )
    reg._last_fetch = time.monotonic()
    reg._instances["good"] = _FakeSearchProvider("good", results=[{"title": "legacy"}])

    app = _build_search_app(reg)
    with TestClient(app) as client:
        resp = client.post("/v1/search", json={"query": "q"})
    assert resp.status_code == 200
    assert resp.json()["results"] == [{"title": "legacy"}]


# ── /v1/audio/speech: X-Opex-Voice header vs body.voice ───────────────────────

def _build_tts_registry_with_fake_provider() -> tuple[ProviderRegistry, _FakeTTSProvider]:
    """A registry with one enabled tts provider whose instance is our fake,
    so aget_active('tts') resolves to it without any network refresh."""
    reg = ProviderRegistry()
    _skip_network(reg)
    reg.config.providers["tts-fake"] = ProviderConfig(type="tts", driver="minimax", enabled=True)
    fake = _FakeTTSProvider("tts-fake")
    reg._instances["tts-fake"] = fake
    return reg, fake


def test_speech_uses_header_voice_when_body_voice_absent():
    """body.voice is absent → the handler must fall through to X-Opex-Voice."""
    reg, fake = _build_tts_registry_with_fake_provider()
    app = _build_tts_app(reg)
    with TestClient(app) as client:
        resp = client.post(
            "/v1/audio/speech",
            json={"input": "hello"},
            headers={"X-Opex-Voice": "TestVoice"},
        )
    assert resp.status_code == 200
    assert len(fake.calls) == 1
    assert fake.calls[0]["voice"] == "TestVoice"


def test_speech_body_voice_overrides_header_voice():
    """body.voice is non-empty → it wins over X-Opex-Voice (body wins)."""
    reg, fake = _build_tts_registry_with_fake_provider()
    app = _build_tts_app(reg)
    with TestClient(app) as client:
        resp = client.post(
            "/v1/audio/speech",
            json={"input": "hello", "voice": "BodyVoice"},
            headers={"X-Opex-Voice": "HeaderVoice"},
        )
    assert resp.status_code == 200
    assert len(fake.calls) == 1
    assert fake.calls[0]["voice"] == "BodyVoice"


def test_speech_no_header_no_body_voice_falls_back_to_empty_string():
    """Neither body.voice nor the header is set → provider gets "" (its own
    default-voice logic takes over), matching `body.voice or header or ""`."""
    reg, fake = _build_tts_registry_with_fake_provider()
    app = _build_tts_app(reg)
    with TestClient(app) as client:
        resp = client.post("/v1/audio/speech", json={"input": "hello"})
    assert resp.status_code == 200
    assert len(fake.calls) == 1
    assert fake.calls[0]["voice"] == ""


# ── registry.aget_active: uninstantiated (disabled) active-id falls through ──

@pytest.mark.asyncio
async def test_aget_active_falls_back_when_active_id_is_disabled_provider():
    """`active={"tts": "tts-off"}` names a DISABLED provider — it never gets
    an entry in `_instances`, so aget_active must fall through to the
    category-matched enabled provider rather than returning None."""
    reg = _make_registry(
        providers={
            "tts-off": {"type": "tts", "driver": "minimax", "enabled": False},
            "tts-a": {"type": "tts", "driver": "minimax", "enabled": True},
        },
        active={"tts": "tts-off"},
    )
    p = await reg.aget_active("tts")
    assert p is not None
    assert p is reg._instances["tts-a"]
