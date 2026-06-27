"""OpenAI Whisper STT provider."""

import httpx

from providers.base import resolve_request_timeout


def _segments_to_timestamped_text(payload: dict) -> str:
    """Fold a verbose_json transcription response into a `[MM:SS] text` transcript.

    faster-whisper / OpenAI verbose_json returns a `segments` list with float
    `start` seconds. Prefixing each segment with its timecode gives the digest
    LLM real time anchors (so it can write timecoded headings and place frames).
    Falls back to the plain `text` field when no usable segments are present."""
    segments = payload.get("segments") or []
    lines: list[str] = []
    for seg in segments:
        text = (seg.get("text") or "").strip()
        if not text:
            continue
        start = int(seg.get("start") or 0)
        lines.append(f"[{start // 60:02d}:{start % 60:02d}] {text}")
    if lines:
        return "\n".join(lines)
    return payload.get("text", "")


class OpenAISTT:
    name = "OpenAI Whisper"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.openai.com/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "whisper-1"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def transcribe(self, http: httpx.AsyncClient, audio_bytes: bytes,
                         filename: str, language: str,
                         model: str | None = None) -> str:
        # Omit the Authorization header entirely when no api_key is set: a local
        # OpenAI-compatible server (e.g. speaches) needs no auth, and an empty
        # `Bearer ` value is rejected by httpx ("Illegal header value b'Bearer '").
        headers = {"Authorization": f"Bearer {self.api_key}"} if self.api_key else {}
        # Request verbose_json to get per-segment timestamps and fold them into the
        # transcript as `[MM:SS]` markers. This gives the downstream digest LLM
        # explicit time anchors so it can write timecoded section headings and
        # align frames — the plain `text` response has no time information.
        resp = await http.post(
            f"{self.base_url}/audio/transcriptions",
            headers=headers,
            files={"file": (filename, audio_bytes, "audio/ogg")},
            data={"model": model or self.model, "language": language,
                  "response_format": "verbose_json"},
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        payload = resp.json()
        return _segments_to_timestamped_text(payload)
