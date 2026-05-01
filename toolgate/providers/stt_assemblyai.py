"""AssemblyAI STT provider — high-accuracy transcription."""

import asyncio

import httpx

from providers.base import resolve_request_timeout


class AssemblyAISTT:
    name = "AssemblyAI"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "https://api.assemblyai.com").rstrip("/")
        self.api_key = api_key or ""
        self.model = model or "universal-2"
        opts = options or {}
        self.language_detection = opts.get("language_detection", True)
        self.speaker_diarization = opts.get("speaker_diarization", False)
        self._request_timeout = resolve_request_timeout(opts)

    async def transcribe(self, http: httpx.AsyncClient, audio_bytes: bytes,
                         filename: str, language: str,
                         model: str | None = None) -> str:
        model_name = model or self.model

        upload_resp = await http.post(
            f"{self.base_url}/v2/upload",
            headers={"Authorization": self.api_key},
            content=audio_bytes,
            timeout=self._request_timeout,
        )
        upload_resp.raise_for_status()
        upload_url = upload_resp.json()["upload_url"]

        payload: dict = {
            "audio_url": upload_url,
            "speech_models": [model_name],
        }
        if language and language != "auto":
            payload["language_code"] = language
        elif self.language_detection:
            payload["language_detection"] = True
        if self.speaker_diarization:
            payload["speaker_labels"] = True

        transcript_resp = await http.post(
            f"{self.base_url}/v2/transcript",
            headers={"Authorization": self.api_key, "Content-Type": "application/json"},
            json=payload,
            timeout=self._request_timeout,
        )
        transcript_resp.raise_for_status()
        transcript_id = transcript_resp.json()["id"]

        for _ in range(120):
            poll_resp = await http.get(
                f"{self.base_url}/v2/transcript/{transcript_id}",
                headers={"Authorization": self.api_key},
                timeout=self._request_timeout,
            )
            poll_resp.raise_for_status()
            result = poll_resp.json()
            status = result["status"]

            if status == "completed":
                return result.get("text", "")
            elif status == "error":
                raise Exception(f"AssemblyAI error: {result.get('error', 'unknown')}")

            await asyncio.sleep(0.5)

        raise Exception("AssemblyAI transcription timed out after 60s")
