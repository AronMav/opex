import asyncio
import json
import os
import subprocess
import tempfile

import httpx
import pytest
from fastapi.testclient import TestClient

import video_helpers
from video_helpers import (
    extract_audio,
    extract_scene_frames,
    extract_uniform_frames,
    download_video,
    detect_scene_cuts,
    _avoid_cuts,
)


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


# ── Scene-avoidance frame extraction unit tests ──────────────────────────────

@pytest.mark.asyncio
async def test_detect_scene_cuts_parses_pts_time(monkeypatch):
    """detect_scene_cuts parses pts_time: floats from ffmpeg stderr (sorted)."""
    stderr = (
        b"[Parsed_showinfo] n:0 pts:120 pts_time:5.0 pos:1\n"
        b"some noise without a timestamp\n"
        b"[Parsed_showinfo] n:1 pts:24 pts_time:1.0 pos:2\n"
        b"[Parsed_showinfo] n:2 pts:300 pts_time:12.5 pos:3\n"
        b"[Parsed_showinfo] garbage pts_time:notanumber pos:4\n"
    )

    async def fake_run(*args):
        # Verify the scene-detect filter is wired up correctly.
        assert "showinfo" in " ".join(args)
        return 0, b"", stderr
    monkeypatch.setattr(video_helpers, "_run", fake_run)

    cuts = await detect_scene_cuts("dummy.mp4")
    assert cuts == [1.0, 5.0, 12.5]


@pytest.mark.asyncio
async def test_detect_scene_cuts_returns_empty_on_ffmpeg_error(monkeypatch):
    """ffmpeg non-zero exit → empty list (graceful degradation, no raise)."""
    async def fake_run(*args):
        return 1, b"", b"ffmpeg blew up"
    monkeypatch.setattr(video_helpers, "_run", fake_run)
    assert await detect_scene_cuts("dummy.mp4") == []


def test_avoid_cuts_no_cuts_returns_unchanged():
    assert _avoid_cuts(10.0, [], duration=60.0) == 10.0


def test_avoid_cuts_far_from_cut_returns_unchanged():
    # Cut at 30s, ts at 10s — well outside the ±gap window.
    assert _avoid_cuts(10.0, [30.0], duration=60.0, gap=2.0) == 10.0


def test_avoid_cuts_wide_window_returns_midpoint():
    # ts=10 sits near cut at 9; surrounding window (9, 40) is wider than 2*gap=4,
    # so the corrected ts is the window midpoint (9+40)/2 = 24.5.
    ts = _avoid_cuts(10.0, [9.0, 40.0], duration=60.0, gap=2.0)
    assert ts == pytest.approx(24.5)
    # And it is no longer within gap of any cut.
    assert all(abs(ts - c) > 2.0 for c in [9.0, 40.0])


def test_avoid_cuts_narrow_window_shifts_by_gap():
    # Cuts at 9 and 12 → window width 3 < 2*gap=4. prev+gap = 11, but that is
    # > next-gap (10), so it falls back to next-gap = 10.
    ts = _avoid_cuts(10.5, [9.0, 12.0], duration=60.0, gap=2.0)
    assert ts == pytest.approx(10.0)


def test_avoid_cuts_clamps_to_video_bounds():
    # A cut at 0.5 near ts=1, no right cut → window (0.5, duration). Midpoint may
    # be huge but stays within [0, duration]; just assert it never escapes bounds.
    ts = _avoid_cuts(1.0, [0.5], duration=20.0, gap=2.0)
    assert 0.0 <= ts <= 20.0


@pytest.mark.asyncio
async def test_extract_uniform_frames_avoids_scene_cuts(monkeypatch):
    """Candidate timestamps near scene cuts are nudged out of the ±gap window."""
    duration = 100.0
    cuts = [25.0, 50.0, 75.0]
    gap = 2.0

    # ── Mock ffprobe (duration) + detect_scene_cuts + per-frame ffmpeg ──
    async def fake_run(*args):
        if args[0] == "ffprobe":
            return 0, f"{duration}".encode(), b""
        # ffmpeg frame extraction: write the expected output file so the
        # extractor reads it back.  out_path is the last positional arg.
        out_path = args[-1]
        with open(out_path, "wb") as f:
            f.write(b"\xff\xd8\xff")  # tiny JPEG-ish blob
        return 0, b"", b""
    monkeypatch.setattr(video_helpers, "_run", fake_run)

    async def fake_cuts(path, threshold=0.3):
        return cuts
    monkeypatch.setattr(video_helpers, "detect_scene_cuts", fake_cuts)

    frames = await extract_uniform_frames("dummy.mp4", count=8)

    assert frames, "expected some frames"
    timestamps = [ts for ts, _ in frames]
    # No emitted timestamp may sit within `gap` of any detected cut.
    for ts in timestamps:
        for c in cuts:
            assert abs(ts - c) >= gap, f"ts {ts} too close to cut {c}"
    # Sorted by time.
    assert timestamps == sorted(timestamps)


@pytest.mark.asyncio
async def test_extract_uniform_frames_dedups_close_timestamps(monkeypatch):
    """Two base points nudged into the same stable point collapse to one frame."""
    duration = 30.0
    # A single wide gap with cuts that funnel several base points to one midpoint.
    cuts = [5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0]

    async def fake_run(*args):
        if args[0] == "ffprobe":
            return 0, f"{duration}".encode(), b""
        out_path = args[-1]
        with open(out_path, "wb") as f:
            f.write(b"\xff\xd8\xff")
        return 0, b"", b""
    monkeypatch.setattr(video_helpers, "_run", fake_run)

    async def fake_cuts(path, threshold=0.3):
        return cuts
    monkeypatch.setattr(video_helpers, "detect_scene_cuts", fake_cuts)

    frames = await extract_uniform_frames("dummy.mp4", count=10)
    timestamps = [ts for ts, _ in frames]
    # No two emitted timestamps may be within 1.0s of each other (dedup invariant).
    for a, b in zip(timestamps, timestamps[1:]):
        assert b - a >= 1.0, f"timestamps {a} and {b} not deduped"


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


# ── YouTube video-id extraction (cache key + traversal guard) ────────────────

def test_youtube_video_id_watch_param():
    from routers.video import _youtube_video_id
    assert _youtube_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ") == "dQw4w9WgXcQ"
    # Extra query params don't interfere.
    assert _youtube_video_id("https://youtube.com/watch?v=dQw4w9WgXcQ&t=42s&list=PL1") == "dQw4w9WgXcQ"
    # m.youtube.com / music.youtube.com share the watch?v= shape.
    assert _youtube_video_id("https://m.youtube.com/watch?v=abcDEF12345") == "abcDEF12345"


def test_youtube_video_id_short_url():
    from routers.video import _youtube_video_id
    assert _youtube_video_id("https://youtu.be/dQw4w9WgXcQ") == "dQw4w9WgXcQ"
    # Trailing path / query after the id is ignored.
    assert _youtube_video_id("https://youtu.be/dQw4w9WgXcQ?t=10") == "dQw4w9WgXcQ"


def test_youtube_video_id_garbage_returns_none():
    from routers.video import _youtube_video_id
    assert _youtube_video_id("") is None
    assert _youtube_video_id("not a url at all") is None
    assert _youtube_video_id("https://example.com/page") is None
    # No v= param.
    assert _youtube_video_id("https://www.youtube.com/feed/subscriptions") is None
    # Wrong-length id (10 chars, not 11) is rejected by the strict shape check.
    assert _youtube_video_id("https://youtu.be/short") is None
    assert _youtube_video_id("https://www.youtube.com/watch?v=tooLongVideoId123") is None


def test_youtube_video_id_traversal_attempt_returns_none():
    from routers.video import _youtube_video_id
    # A path-traversal payload in v= must NOT pass — it would otherwise become a
    # cache filename. `/` and `.` are outside [A-Za-z0-9_-], so it fails the regex.
    assert _youtube_video_id("https://www.youtube.com/watch?v=../../etc") is None
    assert _youtube_video_id("https://www.youtube.com/watch?v=../../../etc/passwd") is None
    assert _youtube_video_id("https://youtu.be/../../etc/passwd") is None


# ── Raw-material cache round-trip ─────────────────────────────────────────────

class _CacheVision:
    name = "v"
    async def describe(self, http, image_bytes, ct, prompt, max_tokens=2000):
        return '{"score": 5, "description": "ok"}'


class _CacheSTT:
    name = "s"
    async def transcribe(self, http, audio_bytes, fn, lang, model=None):
        return "кэш-транскрипт"


def _wire_cache_test(monkeypatch, cache_dir, fetch_calls):
    """Common setup: tmp cache dir, fake providers, and a _materialize_source that
    counts invocations (so we can prove a cache hit skips the download)."""
    import app as toolgate_app
    import routers.video as video_mod

    monkeypatch.setattr(toolgate_app, "AUTH_TOKEN", "")
    monkeypatch.setattr(video_mod, "VIDEO_CACHE_DIR", cache_dir)

    async def fake_active(cap):
        return _CacheSTT() if cap == "stt" else _CacheVision()
    monkeypatch.setattr(toolgate_app.registry, "aget_active", fake_active)

    async def fake_fetch(http, url, work_dir):
        fetch_calls.append(url)
        vid = os.path.join(work_dir, "v.mp4")
        _make_tiny_video(vid)
        return vid
    monkeypatch.setattr(video_mod, "_materialize_source", fake_fetch)
    return toolgate_app


def test_cache_round_trip_second_call_skips_download(monkeypatch):
    """Two page_url requests for the same YouTube id: the second is served from
    cache without re-invoking _materialize_source (download/STT/vision skipped)."""
    page_url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    fetch_calls: list[str] = []
    with tempfile.TemporaryDirectory() as cache_dir:
        toolgate_app = _wire_cache_test(monkeypatch, cache_dir, fetch_calls)
        with TestClient(toolgate_app.app) as client:
            r1 = client.post("/summarize-video", json={"page_url": page_url, "language": "ru"})
            assert r1.status_code == 200, r1.text
            assert len(fetch_calls) == 1, "first call must run the full pass"

            r2 = client.post("/summarize-video", json={"page_url": page_url, "language": "ru"})
            assert r2.status_code == 200, r2.text
            # No new fetch — served from cache.
            assert len(fetch_calls) == 1, "second call must be a cache hit (no download)"

        # Both responses carry identical raw material.
        assert r1.json()["transcript"] == "кэш-транскрипт"
        assert r2.json() == r1.json()
        # Cache file exists on disk keyed by video-id.
        assert os.path.exists(os.path.join(cache_dir, "dQw4w9WgXcQ.json"))


def test_cache_reuse_false_forces_full_run(monkeypatch):
    """reuse_cache=False bypasses an existing cache and re-runs the full pass."""
    page_url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    fetch_calls: list[str] = []
    with tempfile.TemporaryDirectory() as cache_dir:
        toolgate_app = _wire_cache_test(monkeypatch, cache_dir, fetch_calls)
        with TestClient(toolgate_app.app) as client:
            # Prime the cache.
            r1 = client.post("/summarize-video", json={"page_url": page_url})
            assert r1.status_code == 200
            assert len(fetch_calls) == 1

            # reuse_cache=False → full run even though the cache file exists.
            r2 = client.post("/summarize-video", json={"page_url": page_url, "reuse_cache": False})
            assert r2.status_code == 200
            assert len(fetch_calls) == 2, "reuse_cache=False must force a download"


def test_cache_not_used_for_upload(monkeypatch):
    """video_url (upload) sources never touch the cache — each call runs fully and
    no cache file is written."""
    fetch_calls: list[str] = []
    with tempfile.TemporaryDirectory() as cache_dir:
        toolgate_app = _wire_cache_test(monkeypatch, cache_dir, fetch_calls)
        with TestClient(toolgate_app.app) as client:
            r1 = client.post("/summarize-video", json={"video_url": "http://localhost/api/uploads/x"})
            assert r1.status_code == 200
            r2 = client.post("/summarize-video", json={"video_url": "http://localhost/api/uploads/x"})
            assert r2.status_code == 200
        # Both calls ran the full pass; nothing cached.
        assert len(fetch_calls) == 2
        assert os.listdir(cache_dir) == [], "upload sources must not write a cache file"


def test_cache_unparseable_video_id_skips_cache(monkeypatch):
    """A page_url with no extractable YouTube id runs fully every time and never
    writes a cache file (no key to write under)."""
    fetch_calls: list[str] = []
    with tempfile.TemporaryDirectory() as cache_dir:
        toolgate_app = _wire_cache_test(monkeypatch, cache_dir, fetch_calls)
        with TestClient(toolgate_app.app) as client:
            r1 = client.post("/summarize-video", json={"page_url": "https://example.com/video"})
            assert r1.status_code == 200
            r2 = client.post("/summarize-video", json={"page_url": "https://example.com/video"})
            assert r2.status_code == 200
        assert len(fetch_calls) == 2
        assert os.listdir(cache_dir) == [], "no video-id → no cache file"
