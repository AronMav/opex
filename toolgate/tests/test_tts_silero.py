"""Unit tests for the SileroTTS provider (toolgate)."""

import json

import httpx
import pytest
import respx

from providers.tts_silero import SileroTTS, SILERO_SPEAKERS


@pytest.mark.asyncio
async def test_synthesize_posts_openai_shape_and_does_not_normalize(http_client):
    """Driver forwards raw text (numbers/units intact) in the OpenAI request shape.
    The Silero service normalizes internally — the driver must NOT pre-normalize."""
    tts = SileroTTS(base_url="http://silero-test", options={"voice": "kseniya"})
    async with respx.mock(assert_all_called=True) as mock:
        route = mock.post("http://silero-test/v1/audio/speech").mock(
            return_value=httpx.Response(200, content=b"OGG")
        )
        out = await tts.synthesize(
            http_client, "Дано 5 кг", voice="baya", response_format="opus"
        )
    assert out == b"OGG"
    sent = json.loads(route.calls.last.request.content)
    assert sent["input"] == "Дано 5 кг"          # left intact — no normalization here
    assert sent["voice"] == "baya"
    assert sent["response_format"] == "opus"
    assert sent["model"] == "v5_1_ru"


@pytest.mark.asyncio
async def test_voice_falls_back_to_default(http_client):
    tts = SileroTTS(base_url="http://silero-test", options={"voice": "kseniya"})
    async with respx.mock(assert_all_called=True) as mock:
        route = mock.post("http://silero-test/v1/audio/speech").mock(
            return_value=httpx.Response(200, content=b"A")
        )
        await tts.synthesize(http_client, "Текст", voice="", response_format="mp3")
    assert json.loads(route.calls.last.request.content)["voice"] == "kseniya"


@pytest.mark.asyncio
async def test_synthesize_retries_once_on_5xx(http_client):
    """First call after container start loads the model; a 5xx must be retried once."""
    tts = SileroTTS(base_url="http://silero-test", options={"voice": "kseniya"})
    async with respx.mock() as mock:
        route = mock.post("http://silero-test/v1/audio/speech")
        route.side_effect = [
            httpx.Response(503, text="loading model"),
            httpx.Response(200, content=b"AUDIO"),
        ]
        out = await tts.synthesize(http_client, "Привет", voice="kseniya")
    assert out == b"AUDIO"
    assert len(route.calls) == 2  # one 5xx + one success — proves the retry fired


@pytest.mark.asyncio
async def test_list_voices_returns_speakers(http_client):
    tts = SileroTTS(base_url="http://silero-test", options={"voice": "xenia"})
    res = await tts.list_voices(http_client)
    assert res["voices"] == SILERO_SPEAKERS
    assert res["default"] == "xenia"


def test_accepts_registry_kwarg():
    """Router passes registry= to synthesize(); the provider must accept it."""
    import inspect
    sig = inspect.signature(SileroTTS.synthesize)
    assert "registry" in sig.parameters
