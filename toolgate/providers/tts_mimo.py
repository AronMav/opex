"""Xiaomi MiMo TTS provider.

Unlike OpenAI's /v1/audio/speech, MiMo exposes TTS via the chat-completions
multimodal output shape: POST /v1/chat/completions with `modalities:["audio"]`
and the input text placed in an `assistant`-role message. The audio is
returned base64-encoded inside `choices[0].message.audio.data`.

Voices available (as of 2026-06): `mimo_default, 冰糖, 茉莉, 苏打, 白桦,
Mia, Chloe, Milo, Dean`. Voice clone and voice design require separate
model IDs (`mimo-v2.5-tts-voiceclone` / `mimo-v2.5-tts-voicedesign`).
"""

import base64

import httpx

from providers.base import resolve_request_timeout


class MiMoTTS:
    name = "Xiaomi MiMo TTS"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.xiaomimimo.com").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "mimo-v2.5-tts"
        opts = options or {}
        self.default_voice = opts.get("voice", "Chloe")
        self._request_timeout = resolve_request_timeout(opts)

    async def synthesize(self, http: httpx.AsyncClient, text: str,
                         voice: str, model: str | None = None,
                         response_format: str = "mp3",
                         registry=None) -> bytes:
        resp = await http.post(
            f"{self.base_url}/v1/chat/completions",
            headers={"Authorization": f"Bearer {self.api_key}"},
            json={
                "model": model or self.model,
                "messages": [{"role": "assistant", "content": text}],
                "modalities": ["audio"],
                "audio": {
                    "voice": voice or self.default_voice,
                    "format": response_format,
                },
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        audio_b64 = (
            data.get("choices", [{}])[0]
            .get("message", {})
            .get("audio", {})
            .get("data")
        )
        if not audio_b64:
            raise RuntimeError(f"mimo tts: no audio.data in response: {data}")
        return base64.b64decode(audio_b64)
