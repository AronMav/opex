"""Murf AI TTS provider — high-quality AI voice generator."""

import base64

import httpx

from helpers import validate_url_ssrf
from providers.base import resolve_request_timeout


class MurfTTS:
    name = "Murf AI"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.murf.ai/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "GEN2"
        opts = options or {}
        self.default_voice_id = opts.get("voice_id", "en-US-natalie")
        self.rate = opts.get("rate", 0)
        self.pitch = opts.get("pitch", 0)
        self._request_timeout = resolve_request_timeout(opts)

    async def synthesize(self, http: httpx.AsyncClient, text: str,
                         voice: str, model: str | None = None,
                         response_format: str = "mp3",
                         registry=None) -> bytes:
        voice_id = voice or self.default_voice_id

        format_map = {"mp3": "MP3", "wav": "WAV", "flac": "FLAC", "opus": "OGG"}
        output_format = format_map.get(response_format, "MP3")

        payload: dict = {
            "text": text,
            "voiceId": voice_id,
            "modelVersion": model or self.model,
            "format": output_format,
            "encodeAsBase64": True,
        }
        if self.rate:
            payload["rate"] = self.rate
        if self.pitch:
            payload["pitch"] = self.pitch

        resp = await http.post(
            f"{self.base_url}/speech/generate",
            headers={
                "api-key": self.api_key,
                "Content-Type": "application/json",
            },
            json=payload,
            timeout=self._request_timeout,
        )
        resp.raise_for_status()

        data = resp.json()
        audio_b64 = data.get("encodedAudio", "")
        if not audio_b64:
            audio_url = data.get("audioFile", "")
            if audio_url:
                # F079: SSRF-validate the provider-returned URL before fetching
                # via the shared (non-SSRF) client — an operator-configurable /
                # compromised Murf endpoint could otherwise reflect an internal
                # or metadata URL and have toolgate fetch it as "audio".
                validate_url_ssrf(audio_url)
                audio_resp = await http.get(audio_url, timeout=self._request_timeout)
                audio_resp.raise_for_status()
                return audio_resp.content
            raise Exception("No audio in Murf response")

        return base64.b64decode(audio_b64)
