import asyncio
import os
import subprocess
import tempfile

import pytest
from fastapi.testclient import TestClient

from video_helpers import extract_audio, extract_scene_frames, download_video


def _make_tiny_video(path: str):
    """2-second test video with one scene cut (color change at 1s) + a tone."""
    subprocess.run([
        "ffmpeg", "-y",
        "-f", "lavfi", "-i", "color=c=red:s=128x128:d=1",
        "-f", "lavfi", "-i", "color=c=blue:s=128x128:d=1",
        "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
        "-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0[v]",
        "-map", "[v]", "-map", "2:a", "-t", "2", path,
    ], check=True, capture_output=True)


@pytest.mark.asyncio
async def test_extract_audio_returns_nonempty_bytes():
    with tempfile.TemporaryDirectory() as d:
        vid = os.path.join(d, "in.mp4")
        _make_tiny_video(vid)
        audio = await extract_audio(vid)
        assert isinstance(audio, bytes)
        assert len(audio) > 0


@pytest.mark.asyncio
async def test_extract_scene_frames_finds_the_cut():
    with tempfile.TemporaryDirectory() as d:
        vid = os.path.join(d, "in.mp4")
        _make_tiny_video(vid)
        frames = await extract_scene_frames(vid, threshold=0.3, ceiling=100)
        assert len(frames) >= 1, "the red→blue cut must produce at least one frame"
        ts, jpeg = frames[0]
        assert isinstance(ts, float)
        assert jpeg[:2] == b"\xff\xd8", "JPEG SOI marker"


class _FakeSTT:
    name = "fake-stt"
    async def transcribe(self, http, audio_bytes, filename, language, model=None):
        return "привет это тест"


class _FakeVision:
    name = "fake-vision"
    async def describe(self, http, image_bytes, content_type, prompt, max_tokens=2000):
        return "кадр: синий экран"


def test_summarize_video_local_file(monkeypatch):
    import app as toolgate_app
    # Bypass auth (internal-network check passes for testclient host).
    monkeypatch.setattr(toolgate_app, "AUTH_TOKEN", "")

    # Fake providers via the registry.
    async def fake_active(cap):
        return _FakeSTT() if cap == "stt" else _FakeVision()
    monkeypatch.setattr(toolgate_app.registry, "aget_active", fake_active)

    # Serve a local file path to the router by faking _materialize_source.
    with tempfile.TemporaryDirectory() as d:
        vid = os.path.join(d, "v.mp4")
        _make_tiny_video(vid)

        import routers.video as video_mod
        async def fake_fetch(http, url, work_dir):
            return vid
        monkeypatch.setattr(video_mod, "_materialize_source", fake_fetch)

        # Use context manager so lifespan runs and app.state.http_client is set.
        with TestClient(toolgate_app.app) as client:
            r = client.post("/summarize-video", json={"video_url": "http://localhost/api/uploads/x", "language": "ru"})
        assert r.status_code == 200, r.text
        body = r.json()
        assert body["transcript"] == "привет это тест"
        assert len(body["frames"]) >= 1
        assert body["frames"][0]["description"] == "кадр: синий экран"
        assert body["degraded"] == {"stt": False, "vision": False}


@pytest.mark.asyncio
async def test_download_video_rejects_non_http_scheme():
    # argv flag-smuggling / non-http schemes must be rejected before yt-dlp runs.
    with tempfile.TemporaryDirectory() as d:
        for bad in ["-x", "--exec=rm -rf /", "file:///etc/passwd", "ftp://h/x"]:
            with pytest.raises(ValueError):
                await download_video(bad, d)
