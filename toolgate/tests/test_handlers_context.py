"""Unit tests for handlers.context — HandlerContext, ResultBuilder, HandlerFile, SsrfHttpClient."""
import pytest

from handlers.context import (
    build_context,
    HandlerContext,
    HandlerFile,
    HandlerResult,
    ResultBuilder,
    SsrfHttpClient,
)


class _FakeSTT:
    name = "fake-stt"

    async def transcribe(self, http, audio_bytes, filename, language, model=None):
        # the wrapper must inject the SHARED RAW client + forward kwargs
        assert http is _FakeRegistry.sentinel_http
        assert audio_bytes == b"AUDIO"
        assert language == "en"
        return "hello world"


class _FakeRegistry:
    sentinel_http = object()

    def __init__(self, active):
        self._active = active

    async def aget_active(self, capability):
        return self._active.get(capability)


def test_result_builder_text_shape():
    r = ResultBuilder().text("hi")
    assert isinstance(r, HandlerResult)
    assert r.to_dict() == {
        "status": "ok",
        "summary_text": "hi",
        "artifact_urls": [],
        "reason": None,
    }


def test_result_to_dict_emits_exactly_four_keys():
    # R9: Python wire shape is 4 keys; core deserializes this (video_accepted
    # defaults false). Never emit a 5th key.
    assert set(ResultBuilder().text("x").to_dict().keys()) == {
        "status", "summary_text", "artifact_urls", "reason",
    }


def test_result_builder_failed_unsupported_too_large():
    assert ResultBuilder().failed("boom").to_dict()["status"] == "failed"
    assert ResultBuilder().failed("boom").to_dict()["reason"] == "boom"
    assert ResultBuilder().unsupported("nope").to_dict()["status"] == "unsupported"
    assert ResultBuilder().too_large("big").to_dict()["status"] == "too_large"


@pytest.mark.asyncio
async def test_ctx_exposes_raw_client():
    # R12: ctx.http_client_raw is the SHARED client used for provider/byte calls.
    reg = _FakeRegistry({})
    ctx = build_context(reg, _FakeRegistry.sentinel_http)
    assert ctx.http_client_raw is _FakeRegistry.sentinel_http


@pytest.mark.asyncio
async def test_ctx_stt_wrapper_injects_raw_client_and_forwards():
    reg = _FakeRegistry({"stt": _FakeSTT()})
    ctx = build_context(reg, _FakeRegistry.sentinel_http)
    out = await ctx.stt.transcribe(b"AUDIO", language="en")
    assert out == "hello world"


@pytest.mark.asyncio
async def test_ctx_stt_missing_provider_raises():
    reg = _FakeRegistry({})
    ctx = build_context(reg, _FakeRegistry.sentinel_http)
    with pytest.raises(RuntimeError, match="no active stt provider"):
        await ctx.stt.transcribe(b"AUDIO", language="en")


@pytest.mark.asyncio
async def test_ctx_progress_is_noop_without_job_id():
    reg = _FakeRegistry({})
    ctx = build_context(reg, _FakeRegistry.sentinel_http)
    # no job_id → must not raise, must not POST
    await ctx.progress("downloading", 10)


@pytest.mark.asyncio
async def test_ctx_http_is_ssrf_safe_and_blocks_private(monkeypatch):
    import httpx
    import handlers.context as ctxmod

    blocked = {"called_with": None}

    def _fake_validate(url):
        blocked["called_with"] = url
        if "169.254" in url or "127.0.0.1" in url:
            raise ValueError("blocked private/link-local URL")

    # patch the SSRF validator the SsrfHttpClient was wired to
    monkeypatch.setattr(ctxmod, "validate_url_ssrf", _fake_validate)

    def _ok(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=b"ok")

    async with httpx.AsyncClient(transport=httpx.MockTransport(_ok)) as client:
        ctx = build_context(_FakeRegistry({}), client)
        # ctx.http must be the SSRF wrapper, not the raw client
        assert isinstance(ctx.http, SsrfHttpClient)
        # public host passes the validator + reaches the transport
        r = await ctx.http.get("http://example.com/x")
        assert r.status_code == 200
        assert blocked["called_with"] == "http://example.com/x"
        # private host is rejected before any request
        with pytest.raises(ValueError, match="blocked"):
            await ctx.http.get("http://169.254.169.254/latest")


def test_handler_file_fields_with_source_url():
    f = HandlerFile(bytes=b"X", mime="audio/ogg", filename="a.ogg", size=1,
                    source_url="https://youtu.be/abc")
    assert f.bytes == b"X" and f.mime == "audio/ogg" and f.size == 1
    assert f.filename == "a.ogg" and f.source_url == "https://youtu.be/abc"


def test_handler_file_source_url_defaults_none():
    f = HandlerFile(bytes=b"X", mime="text/plain", filename="a.txt", size=1)
    assert f.source_url is None
