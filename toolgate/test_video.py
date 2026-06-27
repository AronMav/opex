import asyncio
import json
import os
import subprocess
import tempfile

import httpx
import pytest
from fastapi.testclient import TestClient

from video_helpers import extract_audio, extract_scene_frames, extract_uniform_frames, download_video


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
    # Returns valid JSON so the vision-scoring path parses correctly.
    async def describe(self, http, image_bytes, content_type, prompt, max_tokens=2000):
        return '{"score": 7, "description": "кадр: синий экран"}'


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
        # Description is extracted from the vision JSON response.
        assert body["frames"][0]["description"] == "кадр: синий экран"
        assert body["degraded"] == {"stt": False, "vision": False}


@pytest.mark.asyncio
async def test_download_video_rejects_non_http_scheme():
    # argv flag-smuggling / non-http schemes must be rejected before yt-dlp runs.
    with tempfile.TemporaryDirectory() as d:
        for bad in ["-x", "--exec=rm -rf /", "file:///etc/passwd", "ftp://h/x"]:
            with pytest.raises(ValueError):
                await download_video(bad, d)


# ── SSRF loopback guard for video_url ───────────────────────────────────────

@pytest.mark.asyncio
async def test_materialize_source_video_url_rejects_non_loopback():
    """video_url must only accept localhost gateway URLs (SSRF guard)."""
    from routers.video import _materialize_source

    with tempfile.TemporaryDirectory() as d:
        for bad_url in [
            "http://169.254.169.254/latest/meta-data",
            "http://evil.com/x",
            "https://internal.corp/secret",
            "http://10.0.0.1/api",
        ]:
            with pytest.raises(ValueError, match="localhost"):
                await _materialize_source(None, bad_url, d)


def test_summarize_video_returns_images_and_title(monkeypatch):
    import app as toolgate_app
    monkeypatch.setattr(toolgate_app, "AUTH_TOKEN", "")
    async def fake_active(cap):
        return _FakeSTT() if cap == "stt" else _FakeVision()
    monkeypatch.setattr(toolgate_app.registry, "aget_active", fake_active)
    with tempfile.TemporaryDirectory() as d:
        vid = os.path.join(d, "v.mp4")
        _make_tiny_video(vid)
        import routers.video as video_mod
        async def fake_fetch(http, url, work_dir):
            return vid
        monkeypatch.setattr(video_mod, "_materialize_source", fake_fetch)
        with TestClient(toolgate_app.app) as client:
            r = client.post("/summarize-video", json={"video_url": "http://localhost/api/uploads/x", "language": "ru", "title": "Тест"})
        assert r.status_code == 200, r.text
        body = r.json()
        assert body["title"] == "Тест"
        assert len(body["frames"]) >= 1
        import base64
        jpeg = base64.b64decode(body["frames"][0]["image_b64"])
        assert jpeg[:2] == b"\xff\xd8", "frame image is JPEG"
        assert len(body["frames"]) <= 24, "note frame cap"


@pytest.mark.asyncio
async def test_materialize_source_video_url_accepts_loopback():
    """video_url with a localhost URL is accepted and the bytes are written to disk.

    Uses a real httpx.AsyncClient backed by MockTransport so the actual http.get
    call inside _materialize_source is exercised (not monkeypatched away).
    This catches both the SSRF self-block bug (C1a) and the max_bytes=None
    TypeError (C1b) that download_limited would have triggered.
    """
    from routers.video import _materialize_source

    fake_video_bytes = b"\x00\x01\x02\x03\x04"
    upload_url = "http://localhost:18789/api/uploads/x?sig=1"

    def transport_handler(request: httpx.Request) -> httpx.Response:
        assert str(request.url) == upload_url, f"unexpected URL: {request.url}"
        return httpx.Response(200, content=fake_video_bytes)

    transport = httpx.MockTransport(transport_handler)
    async with httpx.AsyncClient(transport=transport) as http:
        with tempfile.TemporaryDirectory() as d:
            path = await _materialize_source(http, upload_url, d)
            assert os.path.exists(path), "upload.mp4 was not written"
            with open(path, "rb") as f:
                assert f.read() == fake_video_bytes, "file content mismatch"


# ── Vision-based frame selection unit tests ──────────────────────────────────

def _fake_jpeg(seed: int = 0) -> bytes:
    """Minimal valid-ish JPEG bytes (SOI + seed byte)."""
    return b"\xff\xd8" + bytes([seed & 0xFF])


@pytest.mark.asyncio
async def test_vision_scoring_extracts_candidates(monkeypatch):
    """summarize_video calls extract_uniform_frames with VIDEO_FRAME_CANDIDATES."""
    import routers.video as video_mod

    captured_count: list[int] = []

    async def fake_uniform(path, count):
        captured_count.append(count)
        # Return `count` fake JPEG frames at evenly-spaced timestamps.
        return [(float(i), _fake_jpeg(i)) for i in range(count)]

    monkeypatch.setattr(video_mod, "extract_uniform_frames", fake_uniform)

    class _ScoringVision:
        name = "v"
        async def describe(self, http, image_bytes, ct, prompt, max_tokens=2000):
            # Give every frame the same score so all are equally ranked.
            return '{"score": 5, "description": "ok"}'

    class _FakeSTT2:
        name = "s"
        async def transcribe(self, http, audio_bytes, fn, lang, model=None):
            return ""

    import app as toolgate_app
    monkeypatch.setattr(toolgate_app, "AUTH_TOKEN", "")
    async def fake_active(cap):
        return _FakeSTT2() if cap == "stt" else _ScoringVision()
    monkeypatch.setattr(toolgate_app.registry, "aget_active", fake_active)

    async def fake_fetch(http, url, work_dir):
        # write a real video so extract_audio works
        vid = os.path.join(work_dir, "v.mp4")
        _make_tiny_video(vid)
        return vid
    monkeypatch.setattr(video_mod, "_materialize_source", fake_fetch)

    with TestClient(toolgate_app.app) as client:
        r = client.post("/summarize-video", json={"video_url": "http://localhost/x"})
    assert r.status_code == 200, r.text
    assert captured_count and captured_count[0] == video_mod.VIDEO_FRAME_CANDIDATES


@pytest.mark.asyncio
async def test_vision_scoring_top_n_by_score(monkeypatch):
    """Top VIDEO_NOTE_MAX_FRAMES frames are selected by score, not position."""
    import routers.video as video_mod

    N_CAND = 10
    MAX_FRAMES = 3

    # Assign scores: frames at index 1,5,8 get score=9; rest score=1.
    HIGH_SCORE_IDX = {1, 5, 8}

    async def fake_uniform(path, count):
        return [(float(i), _fake_jpeg(i)) for i in range(N_CAND)]

    monkeypatch.setattr(video_mod, "extract_uniform_frames", fake_uniform)
    monkeypatch.setattr(video_mod, "VIDEO_FRAME_CANDIDATES", N_CAND)
    monkeypatch.setattr(video_mod, "VIDEO_NOTE_MAX_FRAMES", MAX_FRAMES)

    call_idx = [0]

    class _SelectiveVision:
        name = "v"
        async def describe(self, http, image_bytes, ct, prompt, max_tokens=2000):
            # The frame's seed byte (image_bytes[2]) encodes its index.
            idx = image_bytes[2] if len(image_bytes) > 2 else 0
            sc = 9 if idx in HIGH_SCORE_IDX else 1
            return json.dumps({"score": sc, "description": f"frame-{idx}"})

    class _FakeSTT3:
        name = "s"
        async def transcribe(self, http, audio_bytes, fn, lang, model=None):
            return ""

    import app as toolgate_app
    monkeypatch.setattr(toolgate_app, "AUTH_TOKEN", "")
    async def fake_active(cap):
        return _FakeSTT3() if cap == "stt" else _SelectiveVision()
    monkeypatch.setattr(toolgate_app.registry, "aget_active", fake_active)

    async def fake_fetch(http, url, work_dir):
        vid = os.path.join(work_dir, "v.mp4")
        _make_tiny_video(vid)
        return vid
    monkeypatch.setattr(video_mod, "_materialize_source", fake_fetch)

    with TestClient(toolgate_app.app) as client:
        r = client.post("/summarize-video", json={"video_url": "http://localhost/x"})
    assert r.status_code == 200, r.text
    frames = r.json()["frames"]
    assert len(frames) == MAX_FRAMES, f"expected {MAX_FRAMES} frames, got {len(frames)}"
    # All selected frames should be the high-score ones.
    descriptions = {f["description"] for f in frames}
    expected = {f"frame-{i}" for i in HIGH_SCORE_IDX}
    assert descriptions == expected, f"wrong frames selected: {descriptions}"


@pytest.mark.asyncio
async def test_vision_scoring_sorted_by_timestamp(monkeypatch):
    """After scoring, selected frames must be in chronological order."""
    import routers.video as video_mod

    N_CAND = 6
    MAX_FRAMES = 3

    async def fake_uniform(path, count):
        # Timestamps intentionally non-sequential after slicing.
        return [(float(i * 10), _fake_jpeg(i)) for i in range(N_CAND)]

    monkeypatch.setattr(video_mod, "extract_uniform_frames", fake_uniform)
    monkeypatch.setattr(video_mod, "VIDEO_FRAME_CANDIDATES", N_CAND)
    monkeypatch.setattr(video_mod, "VIDEO_NOTE_MAX_FRAMES", MAX_FRAMES)

    # Give frames 0,2,4 score=9, rest score=1 — so top-3 are at times 0,20,40.
    HIGH_IDX = {0, 2, 4}

    class _TimestampVision:
        name = "v"
        async def describe(self, http, image_bytes, ct, prompt, max_tokens=2000):
            idx = image_bytes[2] if len(image_bytes) > 2 else 0
            sc = 9 if idx in HIGH_IDX else 1
            return json.dumps({"score": sc, "description": f"t{idx}"})

    class _FakeSTT4:
        name = "s"
        async def transcribe(self, http, audio_bytes, fn, lang, model=None):
            return ""

    import app as toolgate_app
    monkeypatch.setattr(toolgate_app, "AUTH_TOKEN", "")
    async def fake_active(cap):
        return _FakeSTT4() if cap == "stt" else _TimestampVision()
    monkeypatch.setattr(toolgate_app.registry, "aget_active", fake_active)

    async def fake_fetch(http, url, work_dir):
        vid = os.path.join(work_dir, "v.mp4")
        _make_tiny_video(vid)
        return vid
    monkeypatch.setattr(video_mod, "_materialize_source", fake_fetch)

    with TestClient(toolgate_app.app) as client:
        r = client.post("/summarize-video", json={"video_url": "http://localhost/x"})
    assert r.status_code == 200, r.text
    frames = r.json()["frames"]
    timestamps = [f["timestamp"] for f in frames]
    assert timestamps == sorted(timestamps), f"frames not in chronological order: {timestamps}"


@pytest.mark.asyncio
async def test_vision_scoring_json_parse_fallback(monkeypatch):
    """Garbage vision response → score=5 fallback, not a crash."""
    import routers.video as video_mod

    N_CAND = 4
    MAX_FRAMES = 2

    async def fake_uniform(path, count):
        return [(float(i), _fake_jpeg(i)) for i in range(N_CAND)]

    monkeypatch.setattr(video_mod, "extract_uniform_frames", fake_uniform)
    monkeypatch.setattr(video_mod, "VIDEO_FRAME_CANDIDATES", N_CAND)
    monkeypatch.setattr(video_mod, "VIDEO_NOTE_MAX_FRAMES", MAX_FRAMES)

    class _GarbageVision:
        name = "v"
        async def describe(self, http, image_bytes, ct, prompt, max_tokens=2000):
            # No JSON at all — triggers the parse fallback.
            return "это вообще не JSON"

    class _FakeSTT5:
        name = "s"
        async def transcribe(self, http, audio_bytes, fn, lang, model=None):
            return ""

    import app as toolgate_app
    monkeypatch.setattr(toolgate_app, "AUTH_TOKEN", "")
    async def fake_active(cap):
        return _FakeSTT5() if cap == "stt" else _GarbageVision()
    monkeypatch.setattr(toolgate_app.registry, "aget_active", fake_active)

    async def fake_fetch(http, url, work_dir):
        vid = os.path.join(work_dir, "v.mp4")
        _make_tiny_video(vid)
        return vid
    monkeypatch.setattr(video_mod, "_materialize_source", fake_fetch)

    with TestClient(toolgate_app.app) as client:
        r = client.post("/summarize-video", json={"video_url": "http://localhost/x"})
    assert r.status_code == 200, r.text
    body = r.json()
    # Should still produce frames (score=5 fallback kept all equally ranked).
    assert len(body["frames"]) == MAX_FRAMES
    # Degraded vision flag must NOT be set — vision responded, just with garbage.
    assert body["degraded"]["vision"] is False
