"""Local Silero TTS provider.

Talks to a standalone OpenAI-compatible Silero service (POST /v1/audio/speech).
The Silero service owns ALL text normalization (num2words, abbreviations,
punctuation->pause) and audio encoding (ffmpeg). This provider is a thin
pass-through: it MUST NOT re-normalize text, or numbers get double-expanded.
requires_key=false."""

import asyncio
import logging

import httpx

log = logging.getLogger("toolgate.tts_silero")

# Native Silero ru speakers. OpenAI aliases are mapped server-side; we surface
# the native list for the UI voice picker.
SILERO_SPEAKERS = ["aidar", "baya", "kseniya", "xenia", "eugene"]


class SileroTTS:
    name = "Silero TTS"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "http://localhost:8088").rstrip("/")
        self.model = model or "v5_1_ru"
        opts = options or {}
        self.default_voice = opts.get("voice", "kseniya")
        timeouts = opts.get("timeouts") or {}
        self.request_timeout = (
            float(timeouts["request_secs"])
            if isinstance(timeouts, dict) and timeouts.get("request_secs") is not None
            else None
        )

    async def synthesize(self, http: httpx.AsyncClient, text: str,
                         voice: str, model: str | None = None,
                         response_format: str = "mp3",
                         registry=None) -> bytes:
        payload = {
            "model": model or self.model,
            "input": text,                      # raw — the service normalizes
            "voice": voice or self.default_voice,
            "response_format": response_format,
        }
        kwargs: dict = {"json": payload}
        if self.request_timeout is not None:
            kwargs["timeout"] = self.request_timeout
        url = f"{self.base_url}/v1/audio/speech"
        for attempt in range(2):
            last = attempt == 1
            try:
                resp = await http.post(url, **kwargs)
            except (httpx.TransportError, httpx.TimeoutException) as e:
                if last:
                    raise
                log.warning("Silero TTS request error (%s) — retrying once", e)
                await asyncio.sleep(1.0)
                continue
            if resp.status_code >= 500 and not last:
                log.warning("Silero TTS HTTP %s — retrying once", resp.status_code)
                await asyncio.sleep(1.0)
                continue
            resp.raise_for_status()
            return resp.content
        raise RuntimeError("Silero TTS: retry loop exhausted without a response")

    async def list_voices(self, http: httpx.AsyncClient) -> dict:
        return {"voices": SILERO_SPEAKERS, "default": self.default_voice}
