"""Unit tests for ProviderRegistry degraded-mode flag."""

import importlib

import pytest
from registry import ProviderRegistry
from config import ProvidersConfig, ProviderConfig

from fastapi.testclient import TestClient


# ── helpers ───────────────────────────────────────────────────────────────────

async def _empty_load():
    return ProvidersConfig()


def _degraded_test_client(monkeypatch) -> TestClient:
    """Reload app with an empty provider config and return a TestClient over it."""
    monkeypatch.setattr("registry.aload_config", _empty_load)
    import app as app_module
    importlib.reload(app_module)
    return TestClient(app_module.app)


# ── tests ─────────────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_is_degraded_true_when_no_providers_loaded(monkeypatch):
    monkeypatch.setattr("registry.aload_config", _empty_load)
    reg = ProviderRegistry()
    await reg.aload()
    assert reg.is_degraded() is True


@pytest.mark.asyncio
async def test_is_degraded_false_after_successful_load(monkeypatch):
    async def _populated_load():
        return ProvidersConfig(
            active={"stt": "local-whisper"},
            providers={
                "local-whisper": ProviderConfig(
                    type="stt",
                    driver="whisper-local",
                    base_url="http://localhost:8300/v1",
                    model="faster-whisper-large-v3",
                ),
            },
        )

    monkeypatch.setattr("registry.aload_config", _populated_load)
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
    assert set(body["capabilities"].keys()) == {"stt", "tts", "vision", "imagegen", "embedding"}
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
