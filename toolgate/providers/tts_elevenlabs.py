"""ElevenLabs TTS provider."""

import httpx

from providers.base import resolve_request_timeout


class ElevenLabsTTS:
    name = "ElevenLabs"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.elevenlabs.io/v1").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "eleven_multilingual_v2"
        opts = options or {}
        self.default_voice_id = opts.get("voice_id", "21m00Tcm4TlvDq8ikWAM")  # Rachel
        self.stability = opts.get("stability", 0.5)
        self.similarity_boost = opts.get("similarity_boost", 0.75)
        self.style = opts.get("style", 0.0)
        self._request_timeout = resolve_request_timeout(opts)

    async def synthesize(self, http: httpx.AsyncClient, text: str,
                         voice: str, model: str | None = None,
                         response_format: str = "mp3",
                         registry=None) -> bytes:
        voice_id = voice or self.default_voice_id
        output_format = "mp3_44100_128" if response_format == "mp3" else "opus_48000_64"

        resp = await http.post(
            f"{self.base_url}/text-to-speech/{voice_id}",
            headers={
                "xi-api-key": self.api_key,
                "Content-Type": "application/json",
            },
            params={"output_format": output_format},
            json={
                "text": text,
                "model_id": model or self.model,
                "voice_settings": {
                    "stability": self.stability,
                    "similarity_boost": self.similarity_boost,
                    "style": self.style,
                },
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        return resp.content
