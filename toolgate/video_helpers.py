"""ffmpeg-based audio + scene-frame extraction and yt-dlp download for the
video-summary pipeline. System ffmpeg is required (already used by audio_trim)."""

import asyncio
import glob
import os
import sys
import tempfile


async def _run(*args: str) -> tuple[int, bytes, bytes]:
    proc = await asyncio.create_subprocess_exec(
        *args, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    out, err = await proc.communicate()
    return proc.returncode or 0, out, err


async def extract_audio(video_path: str) -> bytes:
    """Decode the audio track to mono 16 kHz ogg/opus (small, STT-friendly)."""
    with tempfile.TemporaryDirectory() as d:
        out = os.path.join(d, "audio.ogg")
        code, _, err = await _run(
            "ffmpeg", "-y", "-i", video_path,
            "-vn", "-ac", "1", "-ar", "16000", "-c:a", "libopus", "-b:a", "24k",
            out,
        )
        if code != 0 or not os.path.exists(out):
            raise RuntimeError(f"ffmpeg audio extract failed: {err.decode(errors='ignore')[:400]}")
        with open(out, "rb") as f:
            return f.read()


async def extract_scene_frames(
    video_path: str, threshold: float, ceiling: int
) -> list[tuple[float, bytes]]:
    """Extract a JPEG at each scene cut (`select='gt(scene,threshold)'`).
    `ceiling` is a high safety bound, not a product cap."""
    with tempfile.TemporaryDirectory() as d:
        pattern = os.path.join(d, "f_%05d.jpg")
        # showinfo writes pts_time to stderr; we map frame index → timestamp.
        code, _, err = await _run(
            "ffmpeg", "-y", "-i", video_path,
            "-vf", f"select='gt(scene,{threshold})',showinfo",
            "-vsync", "vfr", "-frames:v", str(ceiling), pattern,
        )
        if code != 0:
            raise RuntimeError(f"ffmpeg scene extract failed: {err.decode(errors='ignore')[:400]}")
        times: list[float] = []
        for line in err.decode(errors="ignore").splitlines():
            if "pts_time:" in line:
                try:
                    times.append(float(line.split("pts_time:")[1].split()[0]))
                except (IndexError, ValueError):
                    pass
        frames: list[tuple[float, bytes]] = []
        for i, fp in enumerate(sorted(glob.glob(os.path.join(d, "f_*.jpg")))):
            with open(fp, "rb") as f:
                ts = times[i] if i < len(times) else float(i)
                frames.append((ts, f.read()))
        return frames


async def extract_uniform_frames(video_path: str, count: int) -> list[tuple[float, bytes]]:
    """Extract `count` frames evenly spaced across the video, high JPEG quality.

    Uses ffprobe to get duration, then extracts frames at computed timestamps
    via -ss seek (one ffmpeg call per frame, precise and simple). Each frame is
    encoded as JPEG with -q:v 2 (highest quality) and scaled to 1280px wide
    while preserving aspect ratio.

    Returns [(timestamp_seconds, jpeg_bytes)] sorted by time.
    """
    # ── 1. Get duration ──────────────────────────────────────────────────────
    code, out, err = await _run(
        "ffprobe", "-v", "error", "-show_entries", "format=duration",
        "-of", "default=nw=1:nk=1", video_path,
    )
    if code != 0:
        raise RuntimeError(f"ffprobe duration failed: {err.decode(errors='ignore')[:400]}")
    duration = float(out.decode().strip() or 0.0)
    if duration <= 0:
        raise RuntimeError("ffprobe returned zero/invalid duration")

    if count <= 0:
        return []

    # ── 2. Compute evenly-spaced timestamps ──────────────────────────────────
    # Use midpoint of each interval so first/last frames aren't at 0s/end.
    step = duration / count
    timestamps = [step * (i + 0.5) for i in range(count)]

    # ── 3. Extract each frame with high-quality JPEG + width normalisation ───
    frames: list[tuple[float, bytes]] = []
    with tempfile.TemporaryDirectory() as d:
        for i, ts in enumerate(timestamps):
            out_path = os.path.join(d, f"f_{i:05d}.jpg")
            c, _, e = await _run(
                "ffmpeg", "-y",
                "-ss", f"{ts:.3f}",
                "-i", video_path,
                "-frames:v", "1",
                "-vf", "scale=1280:-1",
                "-q:v", "2",
                out_path,
            )
            if c != 0 or not os.path.exists(out_path):
                # Skip unreadable frame (e.g. seeking past EOF) without aborting.
                continue
            with open(out_path, "rb") as f:
                frames.append((ts, f.read()))

    return sorted(frames, key=lambda x: x[0])


async def download_video(url: str, dest_dir: str) -> str:
    """Download `url` via yt-dlp to a single file under dest_dir. Returns the path.

    Security: only http/https URLs are accepted (rejects `file:`, `-`-prefixed
    flag-smuggling, etc.), and `--` terminates option parsing so the URL can
    never be read as a yt-dlp flag."""
    if not (url.startswith("http://") or url.startswith("https://")):
        raise ValueError("download_video: only http/https URLs are allowed")
    out_tmpl = os.path.join(dest_dir, "dl.%(ext)s")
    # Invoke yt-dlp via the venv interpreter (`python -m yt_dlp`), not a bare
    # `yt-dlp` on PATH: toolgate's PATH does not include the venv's bin/, so a
    # bare name raises FileNotFoundError ("source fetch failed"). `-m yt_dlp`
    # resolves from the venv's site-packages regardless of PATH.
    # `--js-runtimes node`: yt-dlp >=2025 needs a JS runtime for YouTube's nsig
    # challenge (only deno is enabled by default; the host has node, not deno).
    # Without it extraction is deprecated and fails ("No supported JavaScript runtime").
    code, _, err = await _run(
        sys.executable, "-m", "yt_dlp", "--js-runtimes", "node",
        "-f", "best[ext=mp4]/best", "-o", out_tmpl, "--no-playlist", "--", url
    )
    if code != 0:
        raise RuntimeError(f"yt-dlp failed: {err.decode(errors='ignore')[:400]}")
    files = glob.glob(os.path.join(dest_dir, "dl.*"))
    if not files:
        raise RuntimeError("yt-dlp produced no file")
    return files[0]
