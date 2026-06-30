"""Tests for OpenAISTT — streaming (SSE) + non-streaming transcription folding."""

import json

import httpx
import pytest

from providers.stt_openai import (
    OpenAISTT,
    _content_type_for,
    _segments_to_timestamped_text,
)


def _sse(*events: dict) -> bytes:
    """Build an SSE body: one `data: {json}` event per segment-batch, then [DONE]."""
    body = "".join(f"data: {json.dumps(e, ensure_ascii=False)}\n\n" for e in events)
    return (body + "data: [DONE]\n\n").encode("utf-8")


@pytest.mark.asyncio
async def test_streaming_folds_segments_with_absolute_timecodes():
    # Two events arriving over time; `start` is absolute seconds across the audio.
    sse = _sse(
        {"segments": [{"start": 0.0, "end": 5.0, "text": " Привет", "id": 1}]},
        {"segments": [{"start": 65.0, "end": 70.0, "text": " мир", "id": 2}]},
    )

    def handler(request):
        return httpx.Response(200, content=sse,
                              headers={"content-type": "text/event-stream"})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as http:
        stt = OpenAISTT(base_url="http://stt/v1", model="m")  # stream defaults True
        out = await stt.transcribe(http, b"audiobytes", "v.ogg", "ru")

    # 65s -> [01:05]; [DONE] and blank lines ignored; segments folded in order.
    assert out == "[00:00] Привет\n[01:05] мир"


@pytest.mark.asyncio
async def test_streaming_skips_empty_and_malformed_events():
    sse = (
        "data: {bad json}\n\n"                                    # malformed → skipped
        'data: {"segments": [{"start": 12, "text": "  "}]}\n\n'   # empty text → skipped
        'data: {"segments": [{"start": 12, "text": "ровно"}]}\n\n'
        ": keepalive comment\n\n"                                  # non-data line → skipped
        "data: [DONE]\n\n"
    ).encode("utf-8")

    def handler(request):
        return httpx.Response(200, content=sse)

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as http:
        stt = OpenAISTT(base_url="http://stt/v1", model="m")
        out = await stt.transcribe(http, b"a", "v.ogg", "ru")

    assert out == "[00:12] ровно"


@pytest.mark.asyncio
async def test_non_streaming_when_disabled():
    def handler(request):
        return httpx.Response(200, json={
            "text": "плоский",
            "segments": [{"start": 3, "text": "сегмент"}],
        })

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as http:
        stt = OpenAISTT(base_url="http://stt/v1", model="m", options={"stream": False})
        out = await stt.transcribe(http, b"a", "v.ogg", "ru")

    assert out == "[00:03] сегмент"


def test_fold_falls_back_to_plain_text_when_no_segments():
    assert _segments_to_timestamped_text({"text": "only text", "segments": []}) == "only text"


def test_content_type_for_maps_extension():
    assert _content_type_for("audio.ogg") == "audio/ogg"
    assert _content_type_for("audio.webm") == "audio/webm"
    assert _content_type_for("audio.wav") == "audio/wav"
    assert _content_type_for("clip.m4a") == "audio/mp4"
    # Unknown / extensionless → safe octet-stream (never a wrong audio type).
    assert _content_type_for("noext") == "application/octet-stream"


@pytest.mark.asyncio
async def test_transcribe_sends_content_type_from_filename():
    """The multipart file part must declare a type matching the filename — a
    webm fallback sent as audio/ogg makes the STT server answer 415."""
    captured = {}

    def handler(request):
        captured["body"] = request.content
        return httpx.Response(200, content=_sse({"segments": [{"start": 0, "text": "ок"}]}))

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as http:
        stt = OpenAISTT(base_url="http://stt/v1", model="m")
        await stt.transcribe(http, b"audiobytes", "audio.webm", "ru")

    assert b"audio/webm" in captured["body"], "webm upload must be sent as audio/webm"
    assert b"audio/ogg" not in captured["body"], "must not hardcode audio/ogg for a webm file"
