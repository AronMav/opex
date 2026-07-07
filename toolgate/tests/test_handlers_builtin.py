import os

import pytest

from handlers.context import build_context, HandlerFile
from handlers.loader import HandlerRegistry

BUILTIN_DIR = os.path.join(os.path.dirname(__file__), "..", "handlers", "builtin")


class _FakeSTT:
    name = "fake-stt"
    async def transcribe(self, http, audio_bytes, filename, language, model=None):
        # R12: handler must pass the RAW bytes straight through.
        assert audio_bytes == b"AUDIO"
        return f"transcript:{language}"


class _FakeVision:
    name = "fake-vision"
    async def describe(self, http, image_bytes, content_type, prompt, max_tokens=2000):
        assert image_bytes == b"IMG"
        return f"vision:{content_type}"


class _FakeRegistry:
    def __init__(self, active):
        self._active = active
    async def aget_active(self, capability):
        return self._active.get(capability)


def _load(handler_id):
    reg = HandlerRegistry()
    reg.load_all(os.path.abspath(BUILTIN_DIR), None)
    lh = reg.get(handler_id)
    assert lh is not None, f"{handler_id} not registered"
    return lh


def test_all_four_builtins_parse_and_register():
    reg = HandlerRegistry()
    reg.load_all(os.path.abspath(BUILTIN_DIR), None)
    # save/describe/extract_document run inline (sync); transcribe is async so it
    # can also handle URL sources (download + transcribe) like summarize_video.
    expected_exec = {
        "save": "sync",
        "describe": "sync",
        "extract_document": "sync",
        "transcribe": "async",
    }
    for hid, execu in expected_exec.items():
        lh = reg.get(hid)
        assert lh is not None
        assert lh.tier == "builtin"
        assert lh.descriptor.execution == execu, hid


@pytest.mark.asyncio
async def test_save_returns_ok_with_filename():
    lh = _load("save")
    ctx = build_context(_FakeRegistry({}), object())
    f = HandlerFile(bytes=b"X", mime="application/pdf", filename="d.pdf", size=1)
    out = await lh.run(ctx, f, {})
    d = out.to_dict()
    assert d["status"] == "ok"
    # bytes already persisted by core; save just confirms it.
    assert "d.pdf" in d["summary_text"]


@pytest.mark.asyncio
async def test_transcribe_uses_stt_provider_with_raw_bytes():
    lh = _load("transcribe")
    ctx = build_context(_FakeRegistry({"stt": _FakeSTT()}), object())
    f = HandlerFile(bytes=b"AUDIO", mime="audio/ogg", filename="a.ogg", size=5)
    out = await lh.run(ctx, f, {"language": "en"})
    assert out.to_dict()["summary_text"] == "transcript:en"


@pytest.mark.asyncio
async def test_describe_uses_vision_provider_with_raw_bytes():
    lh = _load("describe")
    ctx = build_context(_FakeRegistry({"vision": _FakeVision()}), object())
    f = HandlerFile(bytes=b"IMG", mime="image/png", filename="i.png", size=3)
    out = await lh.run(ctx, f, {})
    assert out.to_dict()["summary_text"] == "vision:image/png"


@pytest.mark.asyncio
async def test_extract_document_parses_plain_text_bytes():
    # R12: extract parses file.bytes in-process (no loopback POST).
    lh = _load("extract_document")
    ctx = build_context(_FakeRegistry({}), object())
    f = HandlerFile(bytes="Привет мир".encode("utf-8"), mime="text/plain",
                    filename="d.txt", size=10)
    out = await lh.run(ctx, f, {})
    d = out.to_dict()
    assert d["status"] == "ok"
    assert "Привет мир" in d["summary_text"]


@pytest.mark.asyncio
async def test_extract_document_respects_max_chars():
    lh = _load("extract_document")
    ctx = build_context(_FakeRegistry({}), object())
    f = HandlerFile(bytes=("A" * 100).encode("utf-8"), mime="text/plain",
                    filename="d.txt", size=100)
    out = await lh.run(ctx, f, {"max_chars": 10})
    assert len(out.to_dict()["summary_text"]) == 10
