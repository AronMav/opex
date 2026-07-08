"""OpenAI Whisper STT provider."""

import json

import httpx

from providers.base import resolve_request_timeout, join_openai_path

# Map a file extension to an audio MIME type. The STT server (speaches) returns
# HTTP 415 when the multipart part's content-type disagrees with the actual
# bytes, so the type must follow the filename — not a hardcoded `audio/ogg`.
# This matters once silence-trim falls back to the original upload, whose
# extension can be webm / wav / m4a rather than ogg.
_AUDIO_MIME_BY_EXT = {
    "ogg": "audio/ogg", "oga": "audio/ogg", "opus": "audio/ogg",
    "webm": "audio/webm", "wav": "audio/wav", "mp3": "audio/mpeg",
    "m4a": "audio/mp4", "mp4": "audio/mp4", "flac": "audio/flac",
}


def _content_type_for(filename: str) -> str:
    """Pick an audio MIME from `filename`'s extension; octet-stream when unknown."""
    ext = filename.rsplit(".", 1)[-1].lower() if "." in filename else ""
    return _AUDIO_MIME_BY_EXT.get(ext, "application/octet-stream")


def _fold_segments(segments: list | None, lines: list[str]) -> None:
    """Append `[MM:SS] text` for each non-empty segment into `lines`.

    `segments` carry absolute `start` seconds (cumulative over the whole audio,
    not per-window), so the timecode is taken directly from `start`."""
    for seg in segments or []:
        text = (seg.get("text") or "").strip()
        if not text:
            continue
        start = int(seg.get("start") or 0)
        lines.append(f"[{start // 60:02d}:{start % 60:02d}] {text}")


def _segments_to_timestamped_text(payload: dict) -> str:
    """Fold a verbose_json transcription response into a `[MM:SS] text` transcript.

    faster-whisper / OpenAI verbose_json returns a `segments` list with float
    `start` seconds. Prefixing each segment with its timecode gives the digest
    LLM real time anchors (so it can write timecoded headings and place frames).
    Falls back to the plain `text` field when no usable segments are present."""
    lines: list[str] = []
    _fold_segments(payload.get("segments"), lines)
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
        # Stream the transcription (SSE, segment-by-segment) by default. A long
        # transcription is computed silently on the server, so a non-streaming
        # request leaves the connection IDLE for minutes — an idle network bridge
        # / NAT / proxy between toolgate and the STT server then drops it and the
        # whole job fails even though the server finished the work. Streaming keeps
        # bytes flowing for the entire job (segments arrive as produced), so no
        # idle timeout can fire. Disable per-provider with options {"stream": false}
        # for backends that don't support streaming transcriptions (e.g. OpenAI
        # whisper-1).
        self._stream = bool(opts.get("stream", True))

    async def transcribe(self, http: httpx.AsyncClient, audio_bytes: bytes,
                         filename: str, language: str,
                         model: str | None = None) -> str:
        # Omit the Authorization header entirely when no api_key is set: a local
        # OpenAI-compatible server (e.g. speaches) needs no auth, and an empty
        # `Bearer ` value is rejected by httpx ("Illegal header value b'Bearer '").
        headers = {"Authorization": f"Bearer {self.api_key}"} if self.api_key else {}
        # verbose_json yields per-segment timestamps, folded into `[MM:SS]` markers
        # so the downstream digest LLM has explicit time anchors.
        files = {"file": (filename, audio_bytes, _content_type_for(filename))}
        data = {"model": model or self.model, "language": language,
                "response_format": "verbose_json"}
        url = join_openai_path(self.base_url, "/v1/audio/transcriptions")

        if not self._stream:
            resp = await http.post(url, headers=headers, files=files, data=data,
                                   timeout=self._request_timeout)
            resp.raise_for_status()
            return _segments_to_timestamped_text(resp.json())

        # Streaming path: read SSE `data: {json}` events and fold segments as they
        # arrive. The connection carries data for the whole job → no idle drop.
        data["stream"] = "true"
        lines: list[str] = []
        async with http.stream("POST", url, headers=headers, files=files,
                               data=data, timeout=self._request_timeout) as resp:
            if resp.status_code >= 400:
                body = await resp.aread()
                raise httpx.HTTPStatusError(
                    f"STT HTTP {resp.status_code}: {body[:300]!r}",
                    request=resp.request, response=resp)
            async for raw in resp.aiter_lines():
                raw = raw.strip()
                if not raw.startswith("data:"):
                    continue
                chunk = raw[len("data:"):].strip()
                if not chunk or chunk == "[DONE]":
                    continue
                try:
                    obj = json.loads(chunk)
                except ValueError:
                    continue
                _fold_segments(obj.get("segments"), lines)
        return "\n".join(lines)
