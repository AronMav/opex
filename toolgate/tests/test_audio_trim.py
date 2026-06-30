"""Unit tests for best-effort STT silence trimming (audio_trim.trim_silence)."""

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from audio_trim import trim_silence


@pytest.mark.asyncio
async def test_empty_input_passthrough():
    """Empty audio short-circuits before ffmpeg and is returned unchanged."""
    out, ext = await trim_silence(b"", "webm")
    assert out == b""
    assert ext == "webm"


@pytest.mark.asyncio
async def test_success_returns_trimmed_ogg():
    proc = MagicMock()
    proc.returncode = 0
    # Real trimmed audio — well above the header-only floor (_MIN_TRIMMED_BYTES).
    trimmed = b"OggS" + b"\xab" * 2000
    proc.communicate = AsyncMock(return_value=(trimmed, b""))
    with patch("audio_trim.asyncio.create_subprocess_exec", AsyncMock(return_value=proc)):
        out, ext = await trim_silence(b"INPUTAUDIO", "webm")
    assert out == trimmed
    assert ext == "ogg"


@pytest.mark.asyncio
async def test_header_only_output_falls_back_to_original():
    """All-silence input → ffmpeg emits a header-only Ogg (no audio frames).
    That tiny output would 500 the STT server (EOFError), so trim_silence must
    fall back to the original upload (keeping its extension)."""
    proc = MagicMock()
    proc.returncode = 0
    proc.communicate = AsyncMock(return_value=(b"OggS" + b"\x00" * 200, b""))  # ~204 bytes
    with patch("audio_trim.asyncio.create_subprocess_exec", AsyncMock(return_value=proc)):
        out, ext = await trim_silence(b"ORIGINAL-WEBM-AUDIO-WITH-A-DECODABLE-STREAM", "webm")
    assert out == b"ORIGINAL-WEBM-AUDIO-WITH-A-DECODABLE-STREAM"
    assert ext == "webm"


@pytest.mark.asyncio
async def test_ffmpeg_nonzero_exit_passthrough():
    """Non-zero ffmpeg exit (or empty output) → original audio + original ext."""
    proc = MagicMock()
    proc.returncode = 1
    proc.communicate = AsyncMock(return_value=(b"", b"some error"))
    with patch("audio_trim.asyncio.create_subprocess_exec", AsyncMock(return_value=proc)):
        out, ext = await trim_silence(b"INPUTAUDIO", "ogg")
    assert out == b"INPUTAUDIO"
    assert ext == "ogg"


@pytest.mark.asyncio
async def test_ffmpeg_missing_passthrough():
    """ffmpeg not installed → graceful passthrough, no raise."""
    with patch("audio_trim.asyncio.create_subprocess_exec", side_effect=FileNotFoundError()):
        out, ext = await trim_silence(b"INPUTAUDIO", "ogg")
    assert out == b"INPUTAUDIO"
    assert ext == "ogg"
