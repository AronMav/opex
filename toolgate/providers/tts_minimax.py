"""MiniMax T2A v2 TTS provider.

MiniMax exposes text-to-speech via POST /v1/t2a_v2 (global endpoint
https://api.minimax.io). Unlike OpenAI's /v1/audio/speech, the request body
carries `voice_setting` / `audio_setting` objects and the synthesized audio is
returned **hex-encoded** at `data.audio` (output_format defaults to "hex").

A `GroupId` query param is optional for the synchronous T2A endpoint (verified
against api.minimax.io) — supply it via `options.group_id` if your account
requires it. Models: speech-2.6-hd (default), speech-2.6-turbo, speech-02-hd,
speech-02-turbo, speech-01-hd, speech-01-turbo. Preset multilingual voices
(Wise_Woman, Deep_Voice_Man, Calm_Woman, Casual_Guy, …) speak Russian on the
2.6 line with `language_boost="auto"`.
"""

import httpx

from providers.base import resolve_request_timeout

# MiniMax accepts these container formats; anything else falls back to mp3.
_SUPPORTED_FORMATS = {"mp3", "wav", "pcm", "flac", "opus"}


class MiniMaxTTS:
    name = "MiniMax T2A"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.minimax.io").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "speech-2.6-hd"
        opts = options or {}
        self.default_voice = opts.get("voice", "Wise_Woman")
        self.group_id = opts.get("group_id") or ""
        self.language_boost = opts.get("language_boost", "auto")
        self.sample_rate = int(opts.get("sample_rate", 32000))
        self.bitrate = int(opts.get("bitrate", 128000))
        self._request_timeout = resolve_request_timeout(opts)

    async def list_voices(self, http: httpx.AsyncClient) -> dict:
        """Return MiniMax preset voices. MiniMax has no /v1/audio/voices
        endpoint, so we return the documented preset list statically."""
        return {
            "voices": [
                "Wise_Woman", "Deep_Voice_Man", "Calm_Woman", "Casual_Guy",
                "Friendly_Person", "Calm_Man", "Serene_Woman", "Young_Knight",
                "Bee_Bee", "MiniMax_001", "MiniMax_002", "presonal_F",
                "presonal_M", "narrator_woman", "narrator_man",
                "clone:Arty",
            ],
            "default": self.default_voice,
        }

    async def synthesize(self, http: httpx.AsyncClient, text: str,
                         voice: str, model: str | None = None,
                         response_format: str = "mp3",
                         registry=None) -> bytes:
        fmt = response_format if response_format in _SUPPORTED_FORMATS else "mp3"
        # MiniMax opus accepts only a subset of sample rates (16000/24000) and
        # rejects 32000/48000 with err 2013. Telegram voice notes (send_voice)
        # request opus, so clamp to a supported rate; opus is returned as a
        # ready OGG-Opus container (magic "OggS"). Other formats keep the
        # configured rate (default 32000).
        sample_rate = 24000 if fmt == "opus" else self.sample_rate
        url = f"{self.base_url}/v1/t2a_v2"
        if self.group_id:
            url = f"{url}?GroupId={self.group_id}"

        body: dict = {
            "model": model or self.model,
            "text": text,
            "output_format": "hex",
            "voice_setting": {
                "voice_id": voice or self.default_voice,
                "speed": 1.0,
                "vol": 1.0,
                "pitch": 0,
            },
            "audio_setting": {
                "sample_rate": sample_rate,
                "bitrate": self.bitrate,
                "format": fmt,
                "channel": 1,
            },
        }
        if self.language_boost:
            body["language_boost"] = self.language_boost

        resp = await http.post(
            url,
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
            json=body,
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()

        # MiniMax returns HTTP 200 even on logical errors — check base_resp.
        base = data.get("base_resp") or {}
        if base.get("status_code") not in (0, None):
            raise RuntimeError(
                f"minimax tts: {base.get('status_msg', 'error')} "
                f"(status_code={base.get('status_code')})"
            )

        audio_hex = (data.get("data") or {}).get("audio")
        if not audio_hex:
            raise RuntimeError(f"minimax tts: no data.audio in response: {data}")
        return bytes.fromhex(audio_hex)
