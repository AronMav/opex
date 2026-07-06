"""ffmpeg-based audio + scene-frame extraction and yt-dlp download for the
video-summary pipeline. System ffmpeg is required (already used by audio_trim)."""

import asyncio
import glob
import os
import sys
import tempfile

from helpers import validate_url_ssrf


async def _run(*args: str) -> tuple[int, bytes, bytes]:
    proc = await asyncio.create_subprocess_exec(
        *args, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    out, err = await proc.communicate()
    return proc.returncode or 0, out, err


# Default to the cookies jar that MeTube maintains on the deploy server; override
# with YTDLP_COOKIES_FILE. YouTube serves a bot-check ("Sign in to confirm you're
# not a bot") to the datacenter/proxy IP for media-stream downloads — a valid
# cookies file authenticates the request and passes the check.
_DEFAULT_COOKIES = os.path.expanduser("~/docker/metube/.metube/cookies.txt")


def _cookie_working_copy() -> str:
    """Owner-only working-copy path in a private per-user temp dir (0o700)."""
    uid = getattr(os, "getuid", lambda: 0)()  # Windows (dev/tests) has no getuid
    d = os.path.join(tempfile.gettempdir(), f"opex_ytdlp_{uid}")
    os.makedirs(d, exist_ok=True)
    try:
        os.chmod(d, 0o700)
    except OSError:
        pass
    return os.path.join(d, "cookies.txt")


def _cookie_args() -> list[str]:
    """`["--cookies", <copy>]` if a cookies file is configured and exists, else `[]`.

    yt-dlp REWRITES the cookies file it is handed (it saves the post-request
    session back). Pointing it at MeTube's shared jar means a bot-checked /
    logged-out response overwrites and DEGRADES that jar — each failed attempt
    makes the next one worse. So copy the jar to a throwaway working file every
    call and give yt-dlp the copy; the source jar is never mutated by us.

    The copy holds session credentials, so it is written owner-only (0o600) in a
    private per-user dir, and created with O_NOFOLLOW after unlinking any existing
    path so a planted symlink can't redirect the write or leak the cookies."""
    path = os.environ.get("YTDLP_COOKIES_FILE", _DEFAULT_COOKIES)
    if not (path and os.path.isfile(path)):
        return []
    try:
        with open(path, "rb") as src:
            data = src.read()
        copy = _cookie_working_copy()
        try:
            os.unlink(copy)
        except FileNotFoundError:
            pass
        flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(copy, flags, 0o600)
        try:
            os.write(fd, data)
        finally:
            os.close(fd)
        return ["--cookies", copy]
    except OSError:
        # Copy failed (disk/perms). Run WITHOUT cookies rather than handing yt-dlp
        # the shared MeTube jar directly — it would rewrite and degrade that jar.
        return []


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


async def detect_scene_cuts(video_path: str, threshold: float = 0.3) -> list[float]:
    """Return timestamps (seconds) of scene cuts via ffmpeg scene detection.

    Runs `select='gt(scene,{threshold})',showinfo` over the whole video and
    parses `pts_time:` from ffmpeg's stderr (same mechanism as
    `extract_scene_frames`, but discards the frames — we only want the cut
    timestamps). Threshold 0.3 is intentionally more sensitive than the default
    0.4 so we catch more transitions and can steer extraction AWAY from them.

    On ANY ffmpeg failure this returns an empty list (never raises): the caller
    then degrades gracefully to pure uniform extraction.
    """
    try:
        code, _, err = await _run(
            "ffmpeg", "-i", video_path,
            "-vf", f"select='gt(scene,{threshold})',showinfo",
            "-f", "null", "-",
        )
    except Exception:
        return []
    if code != 0:
        return []
    times: list[float] = []
    for line in err.decode(errors="ignore").splitlines():
        if "pts_time:" in line:
            try:
                times.append(float(line.split("pts_time:")[1].split()[0]))
            except (IndexError, ValueError):
                pass
    return sorted(times)


def _avoid_cuts(ts: float, cuts: list[float], duration: float, gap: float = 2.0) -> float:
    """Nudge a candidate timestamp away from any nearby scene cut.

    If a scene cut lies within `[ts-gap, ts+gap]`, move `ts` into the middle of
    the nearest stable window between the surrounding cuts:
      - find the closest cut on the left (`prev`, default 0) and right (`next`,
        default `duration`) of `ts`;
      - if that window `(prev, next)` is wider than `2*gap`, return its midpoint
        `(prev+next)/2` (the most stable point);
      - otherwise shift to `prev+gap` (or, if that would overshoot the window,
        `next-gap`), whichever stays inside the video.
    The result is clamped to `[0, duration]`. If no cut is near, `ts` is returned
    unchanged.
    """
    if not cuts:
        return ts
    # Is there a cut within the danger window around ts?
    near = any(ts - gap <= c <= ts + gap for c in cuts)
    if not near:
        return ts
    # Closest cut on each side of ts (window boundaries default to the video ends).
    prev = max((c for c in cuts if c <= ts), default=0.0)
    nxt = min((c for c in cuts if c >= ts), default=duration)
    if nxt - prev > 2 * gap:
        corrected = (prev + nxt) / 2.0
    else:
        # Narrow window: prefer prev+gap, fall back to next-gap if it overshoots.
        corrected = prev + gap
        if corrected > nxt - gap:
            corrected = nxt - gap
    return max(0.0, min(duration, corrected))


async def extract_uniform_frames(video_path: str, count: int) -> list[tuple[float, bytes]]:
    """Extract `count` scene-aware frames spread across the video, high JPEG quality.

    Frames are placed at evenly-spaced midpoints, but each candidate timestamp is
    nudged AWAY from detected scene cuts (`detect_scene_cuts` + `_avoid_cuts`) so
    we never grab a blurry / motion-blurred transition frame. If scene detection
    yields nothing (or ffmpeg fails) this degrades to pure uniform spacing.

    Uses ffprobe for duration, then one `-ss`-seek ffmpeg call per corrected
    timestamp. Each frame is JPEG `-q:v 2` (highest quality), scaled to 1280px
    wide while preserving aspect ratio. Near-duplicate timestamps (two base
    points nudged within <1.0s of each other) are de-duplicated.

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

    # ── 2. Detect scene cuts (best-effort) ───────────────────────────────────
    cuts = await detect_scene_cuts(video_path)

    # ── 3. Compute evenly-spaced midpoints, then steer away from cuts ─────────
    base_ts = [duration * (i + 0.5) / count for i in range(count)]
    corrected: list[float] = []
    for ts in base_ts:
        new_ts = _avoid_cuts(ts, cuts, duration)
        # Dedup: drop a candidate that landed within 1.0s of an already-kept one.
        if any(abs(new_ts - kept) < 1.0 for kept in corrected):
            continue
        corrected.append(new_ts)
    corrected.sort()

    # ── 4. Extract each frame with high-quality JPEG + width normalisation ───
    frames: list[tuple[float, bytes]] = []
    with tempfile.TemporaryDirectory() as d:
        for i, ts in enumerate(corrected):
            out_path = os.path.join(d, f"f_{i:05d}.jpg")
            c, _, e = await _run(
                "ffmpeg", "-y",
                "-ss", f"{ts:.3f}",
                "-i", video_path,
                "-frames:v", "1",
                "-vf", "scale='min(1280,iw)':-1",
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
    never be read as a yt-dlp flag.  validate_url_ssrf is called before yt-dlp
    so SSRF protection holds regardless of which caller reaches this function."""
    if not (url.startswith("http://") or url.startswith("https://")):
        raise ValueError("download_video: only http/https URLs are allowed")
    # SSRF guard: block private/loopback/CGNAT targets before yt-dlp runs.
    # yt-dlp follows redirects internally, so a pre-flight DNS check here is a
    # best-effort defence-in-depth layer; the authoritative SSRF block is still
    # the host allowlist enforced by the caller (Core subagent.rs). This mirrors
    # how download_limited calls validate_url_ssrf before issuing the httpx request.
    validate_url_ssrf(url)
    out_tmpl = os.path.join(dest_dir, "dl.%(ext)s")
    # Invoke yt-dlp via the venv interpreter (`python -m yt_dlp`), not a bare
    # `yt-dlp` on PATH: toolgate's PATH does not include the venv's bin/, so a
    # bare name raises FileNotFoundError ("source fetch failed"). `-m yt_dlp`
    # resolves from the venv's site-packages regardless of PATH.
    # `--js-runtimes deno`: yt-dlp >=2025 needs a JS runtime to solve YouTube's
    # nsig/signature challenge. Deno is yt-dlp's preferred (default) runtime;
    # node is NOT effectively honored ("Only deno is enabled by default") and
    # produces an invalid signature -> media URL returns HTTP 403 / YouTube
    # serves a bot-check ("Sign in to confirm you're not a bot"). Deno must be on
    # toolgate's PATH (~/.local/bin/deno -> ~/.deno/bin/deno on the server).
    # `--remote-components ejs:github`: yt-dlp >=2026.6 requires downloading the
    # "remote components" JS solver from GitHub on first use. Without this flag,
    # yt-dlp warns the solver was "skipped" and extraction fails on newer
    # YouTube player versions.
    code, _, err = await _run(
        sys.executable, "-m", "yt_dlp",
        "--js-runtimes", "deno",
        "--remote-components", "ejs:github",
        *_cookie_args(),
        "-f", "best[ext=mp4]/best", "-o", out_tmpl, "--no-playlist", "--", url
    )
    if code != 0:
        raise RuntimeError(f"yt-dlp failed: {err.decode(errors='ignore')[:400]}")
    files = glob.glob(os.path.join(dest_dir, "dl.*"))
    if not files:
        raise RuntimeError("yt-dlp produced no file")
    return files[0]
