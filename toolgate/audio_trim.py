"""Best-effort silence trimming for STT input audio via ffmpeg.

Mirrors providers/tts_local.py:_ffmpeg_denoise — any failure returns the
original audio so transcription never breaks. On success returns trimmed
Ogg/Opus (compressed — kept small so we never blow past STT size limits the
way uncompressed WAV would on long inputs; accepted by every STT provider).
ffmpeg probes the input container from the stream, so no per-format encoder
table is needed.
"""

import asyncio
import logging

log = logging.getLogger("toolgate.audio_trim")

# Trim leading AND trailing silence: strip leading silence, reverse the stream,
# strip leading silence again (= the original trailing silence), reverse back.
# -50dB is conservative so quiet speech onsets are never clipped.
_SILENCEREMOVE = (
    "silenceremove=start_periods=1:start_threshold=-50dB,"
    "areverse,"
    "silenceremove=start_periods=1:start_threshold=-50dB,"
    "areverse"
)


async def trim_silence(audio: bytes, in_ext: str) -> tuple[bytes, str]:
    """Trim leading/trailing silence from `audio`.

    Returns ``(audio, ext)``:
      * success      → ``(trimmed_ogg_opus_bytes, "ogg")``
      * empty input  → ``(audio, in_ext)`` (no-op)
      * any failure  → ``(original_audio, in_ext)`` (best-effort, never raises)
    """
    if not audio:
        return audio, in_ext
    cmd = [
        "ffmpeg", "-hide_banner", "-loglevel", "error",
        "-i", "pipe:0", "-af", _SILENCEREMOVE,
        "-c:a", "libopus", "-b:a", "32k", "-f", "ogg", "pipe:1",
    ]
    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        out, err = await proc.communicate(input=audio)
        if proc.returncode != 0 or not out:
            log.warning(
                "trim_silence ffmpeg rc=%s: %s",
                proc.returncode,
                err[:300].decode("utf-8", "ignore"),
            )
            return audio, in_ext
        return out, "ogg"
    except FileNotFoundError:
        log.warning("ffmpeg not installed — skipping silence trim")
        return audio, in_ext
    except Exception as e:  # never fail STT because of the trim step
        log.warning("trim_silence error: %s", e)
        return audio, in_ext
