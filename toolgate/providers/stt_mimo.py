"""Xiaomi MiMo ASR (STT) provider.

Uses the chat-completions multimodal input shape: POST /v1/chat/completions
with model=mimo-v2.5-asr and a user message whose content is an
`input_audio` part carrying a `data:<mime>;base64,<bytes>` URI. The
transcribed text is returned as the assistant message content.
"""

import base64

import httpx

from providers.base import resolve_request_timeout


def _mime_from_filename(filename: str) -> str:
    lower = filename.lower()
    if lower.endswith(".mp3"):
        return "audio/mpeg"
    if lower.endswith(".wav"):
        return "audio/wav"
    if lower.endswith(".ogg"):
        return "audio/ogg"
    if lower.endswith(".m4a") or lower.endswith(".aac"):
        return "audio/mp4"
    if lower.endswith(".flac"):
        return "audio/flac"
    if lower.endswith(".webm"):
        return "audio/webm"
    return "audio/ogg"


class MiMoSTT:
    name = "Xiaomi MiMo ASR"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.xiaomimimo.com").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "mimo-v2.5-asr"
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts)

    async def transcribe(self, http: httpx.AsyncClient, audio_bytes: bytes,
                         filename: str, language: str,
                         model: str | None = None) -> str:
        mime = _mime_from_filename(filename)
        b64 = base64.b64encode(audio_bytes).decode("ascii")
        # MiMo ASR officially supports only zh, en, auto. Map anything
        # else (e.g. ru, fr, de) to "auto" so the request is accepted —
        # the model still detects and transcribes other languages.
        lang = (language or "auto").lower()
        if lang not in ("zh", "en", "auto"):
            lang = "auto"
        resp = await http.post(
            f"{self.base_url}/v1/chat/completions",
            headers={"Authorization": f"Bearer {self.api_key}"},
            json={
                "model": model or self.model,
                "messages": [{
                    "role": "user",
                    "content": [{
                        "type": "input_audio",
                        "input_audio": {"data": f"data:{mime};base64,{b64}"},
                    }],
                }],
                "asr_options": {"language": lang},
            },
            timeout=self._request_timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        text = (
            data.get("choices", [{}])[0]
            .get("message", {})
            .get("content", "")
        )
        return text or ""
