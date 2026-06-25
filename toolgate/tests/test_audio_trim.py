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
async def test_success_returns_trimmed_wav():
    proc = MagicMock()
    proc.returncode = 0
    proc.communicate = AsyncMock(return_value=(b"WAVDATA", b""))
    with patch("audio_trim.asyncio.create_subprocess_exec", AsyncMock(return_value=proc)):
        out, ext = await trim_silence(b"INPUTAUDIO", "webm")
    assert out == b"WAVDATA"
    assert ext == "ogg"


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
