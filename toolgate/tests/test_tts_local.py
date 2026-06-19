"""Unit tests for Qwen3TTS normalize-provider resolution."""

from types import SimpleNamespace

import pytest
import respx
import httpx

from providers.tts_local import Qwen3TTS, _ffmpeg_denoise


def test_denoise_option_off_by_default():
    assert Qwen3TTS(base_url="x", options={"voice": "nova"}).denoise is None
    assert Qwen3TTS(base_url="x", options={}).denoise is None


def test_denoise_option_string_and_true():
    assert Qwen3TTS(base_url="x", options={"denoise": "afftdn=nr=10:nf=-45"}).denoise == "afftdn=nr=10:nf=-45"
    assert Qwen3TTS(base_url="x", options={"denoise": True}).denoise == "afftdn=nr=10:nf=-45"
    assert Qwen3TTS(base_url="x", options={"denoise": "  "}).denoise is None


@pytest.mark.asyncio
async def test_ffmpeg_denoise_passes_through_unknown_format():
    """Unknown/raw formats are returned untouched (never corrupted)."""
    data = b"RAWPCMBYTES"
    assert await _ffmpeg_denoise(data, "pcm", "afftdn=nr=10:nf=-45") == data
    assert await _ffmpeg_denoise(b"", "mp3", "afftdn") == b""


@pytest.mark.asyncio
async def test_synthesize_retries_once_on_5xx(http_client):
    """openedai-speech 500s intermittently — one retry must recover the voice."""
    tts = Qwen3TTS(base_url="http://tts-backend", options={"voice": "nova"})
    async with respx.mock() as mock:
        route = mock.post("http://tts-backend/v1/audio/speech")
        route.side_effect = [
            httpx.Response(503, text="busy"),
            httpx.Response(200, content=b"AUDIO"),
        ]
        out = await tts.synthesize(http_client, "Привет", voice="nova", response_format="mp3")
    assert out == b"AUDIO"


class _FakeTextProvider:
    """Stand-in for a resolved text provider returned by registry.aget_instance()."""
    def __init__(self):
        self.base_url = "http://llm-test/v1/chat/completions"
        self.api_key = "sk-test"
        self.model = "test-llm"


class _FakeRegistry:
    def __init__(self, instance_by_id: dict[str, object]):
        self._map = instance_by_id

    async def aget_instance(self, provider_id: str):
        return self._map.get(provider_id)


@pytest.mark.asyncio
async def test_resolves_normalize_provider_id_from_registry(http_client):
    tts = Qwen3TTS(
        base_url="http://tts-backend",
        options={"normalize_provider_id": "abc-123", "normalize": True, "voice": "nova"},
    )
    registry = _FakeRegistry({"abc-123": _FakeTextProvider()})

    async with respx.mock(assert_all_called=True) as mock:
        mock.post("http://llm-test/v1/chat/completions").mock(
            return_value=httpx.Response(
                200, json={"choices": [{"message": {"content": "Привет Пайтон"}}]}
            )
        )
        mock.post("http://tts-backend/v1/audio/speech").mock(
            return_value=httpx.Response(200, content=b"\x00\x01binary")
        )
        audio = await tts.synthesize(
            http_client, "Hello Python", voice="nova", registry=registry
        )
    assert audio == b"\x00\x01binary"


@pytest.mark.asyncio
async def test_missing_normalize_provider_id_falls_back_to_basic_normalize(http_client):
    """When normalize_provider_id is absent, no LLM HTTP call happens but TTS still works."""
    tts = Qwen3TTS(
        base_url="http://tts-backend",
        options={"normalize": True, "voice": "nova"},  # no normalize_provider_id
    )
    registry = _FakeRegistry({})

    async with respx.mock(assert_all_called=False) as mock:
        # LLM endpoint MUST NOT be called
        llm_route = mock.post("http://llm-test/v1/chat/completions").mock(
            return_value=httpx.Response(500)
        )
        mock.post("http://tts-backend/v1/audio/speech").mock(
            return_value=httpx.Response(200, content=b"\x00audio")
        )
        audio = await tts.synthesize(
            http_client, "Hello World", voice="nova", registry=registry
        )
    assert audio == b"\x00audio"
    assert llm_route.call_count == 0


@pytest.mark.asyncio
async def test_unknown_normalize_provider_id_falls_back(http_client, caplog):
    """Reference to a non-existent provider → WARN log, TTS still proceeds."""
    import logging
    caplog.set_level(logging.WARNING)
    tts = Qwen3TTS(
        base_url="http://tts-backend",
        options={"normalize_provider_id": "nonexistent", "normalize": True},
    )
    registry = _FakeRegistry({})  # no mapping for "nonexistent"

    async with respx.mock(assert_all_called=True) as mock:
        mock.post("http://tts-backend/v1/audio/speech").mock(
            return_value=httpx.Response(200, content=b"\x00ok")
        )
        await tts.synthesize(http_client, "Test", voice="nova", registry=registry)
    assert any("normalize_provider_id" in r.message for r in caplog.records)


def test_all_tts_providers_accept_registry_kwarg():
    """All TTS provider implementations must accept registry=None to satisfy the
    router contract. The router passes registry= to provider.synthesize() for any TTS
    capability — providers that don't use it must still accept it gracefully."""
    import inspect
    from providers import tts_local, tts_openai, tts_elevenlabs, tts_edge, tts_fish_audio, tts_murf, tts_silero
    for mod in [tts_local, tts_openai, tts_elevenlabs, tts_edge, tts_fish_audio, tts_murf, tts_silero]:
        cls = next(c for n, c in inspect.getmembers(mod, inspect.isclass) if hasattr(c, "synthesize"))
        sig = inspect.signature(cls.synthesize)
        assert "registry" in sig.parameters, \
            f"{cls.__name__}.synthesize must accept registry kwarg (router passes it)"
