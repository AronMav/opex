import asyncio
import os
import subprocess
import tempfile

import pytest

from video_helpers import extract_audio, extract_scene_frames


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
