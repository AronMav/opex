"""Local Qwen3-TTS provider.

Normalize-LLM credentials come from a separate `type=text` provider
referenced via `options.normalize_provider_id` (UUID). This keeps the
API key in the encrypted vault instead of plaintext JSONB options."""

import asyncio
import logging

import httpx

from normalize import NormalizeLLMConfig, normalize_text


log = logging.getLogger("toolgate.tts_local")


# response_format → ffmpeg output encoder args. Formats not listed are returned
# un-denoised (we won't risk corrupting raw PCM or unknown containers).
_FFMPEG_ENCODE = {
    "opus": ["-c:a", "libopus", "-b:a", "64k", "-f", "ogg"],
    "mp3": ["-c:a", "libmp3lame", "-q:a", "2", "-f", "mp3"],
    "aac": ["-c:a", "aac", "-f", "adts"],
    "flac": ["-c:a", "flac", "-f", "flac"],
    "wav": ["-c:a", "pcm_s16le", "-f", "wav"],
}


async def _ffmpeg_denoise(audio: bytes, response_format: str, af: str) -> bytes:
    """Filter generated audio through ffmpeg (e.g. denoise the XTTS output).

    Best-effort: any failure (ffmpeg missing, unknown format, non-zero exit)
    logs a warning and returns the ORIGINAL audio so TTS never breaks here."""
    enc = _FFMPEG_ENCODE.get((response_format or "mp3").lower())
    if enc is None or not audio:
        return audio
    cmd = ["ffmpeg", "-hide_banner", "-loglevel", "error",
           "-i", "pipe:0", "-af", af, *enc, "pipe:1"]
    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        out, err = await proc.communicate(input=audio)
        if proc.returncode != 0 or not out:
            log.warning("denoise ffmpeg rc=%s: %s", proc.returncode,
                        err[:300].decode("utf-8", "ignore"))
            return audio
        return out
    except FileNotFoundError:
        log.warning("ffmpeg not installed — skipping output denoise")
        return audio
    except Exception as e:  # never fail TTS because of the denoise step
        log.warning("denoise error: %s", e)
        return audio


class Qwen3TTS:
    name = "Qwen3-TTS"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "http://localhost:8880").rstrip("/")
        self.model = model or "tts-1-ru"
        opts = options or {}
        self.default_voice = opts.get("voice", "nova")
        self.normalize = opts.get("normalize", False)
        self.normalize_provider_id: str | None = opts.get("normalize_provider_id") or None
        # Per-provider request timeout override (overrides toolgate's shared
        # 120s client default). Voice-clone warmup + long synth can exceed
        # that — letting operators raise it via UI options.timeouts.request_secs
        # avoids spurious 504s without changing the global client.
        timeouts = opts.get("timeouts") or {}
        self.request_timeout: float | None = (
            float(timeouts["request_secs"])
            if isinstance(timeouts, dict) and timeouts.get("request_secs") is not None
            else None
        )
        # Optional post-synthesis denoise of the TTS output via ffmpeg. XTTS's
        # vocoder leaves a faint hiss that reference-cleaning can't remove, so we
        # filter the generated audio. `options.denoise` is an ffmpeg -af filter
        # string (e.g. "afftdn=nr=10:nf=-45"); `true` selects a sane default.
        denoise_opt = opts.get("denoise")
        if denoise_opt is True:
            self.denoise: str | None = "afftdn=nr=10:nf=-45"
        elif isinstance(denoise_opt, str) and denoise_opt.strip():
            self.denoise = denoise_opt.strip()
        else:
            self.denoise = None

    async def _resolve_llm_config(self, registry) -> NormalizeLLMConfig | None:
        """Resolve normalize-LLM config from the referenced text provider.
        Returns None (caller will fall back to pre/post-only normalize) if:
          - normalize flag is disabled
          - normalize_provider_id is missing
          - referenced provider doesn't exist or lacks base_url/api_key"""
        if not self.normalize:
            return None
        if not self.normalize_provider_id:
            log.warning("normalize=True but no normalize_provider_id configured — "
                        "skipping LLM transliteration (pre/post only)")
            return None
        if registry is None:
            log.warning("no registry passed to synthesize — cannot resolve "
                        "normalize_provider_id=%s", self.normalize_provider_id)
            return None
        text_provider = await registry.aget_instance(self.normalize_provider_id)
        if text_provider is None:
            log.warning("normalize_provider_id=%s not found in registry — "
                        "falling back to basic normalize",
                        self.normalize_provider_id)
            return None
        base_url = getattr(text_provider, "base_url", "")
        api_key = getattr(text_provider, "api_key", "") or ""
        model = getattr(text_provider, "model", "") or ""
        if not base_url or not api_key:
            log.warning("normalize provider %s missing base_url/api_key — "
                        "falling back to basic normalize",
                        self.normalize_provider_id)
            return None
        return NormalizeLLMConfig(base_url=base_url, api_key=api_key, model=model)

    async def synthesize(self, http: httpx.AsyncClient, text: str,
                         voice: str, model: str | None = None,
                         response_format: str = "mp3",
                         registry=None) -> bytes:
        llm_config = await self._resolve_llm_config(registry)
        processed = await normalize_text(http, text, config=llm_config)
        resolved_voice = voice if voice else self.default_voice

        kwargs: dict = {
            "json": {
                "model": model or self.model,
                "input": processed,
                "voice": resolved_voice,
                "response_format": response_format,
            },
        }
        if self.request_timeout is not None:
            kwargs["timeout"] = self.request_timeout
        # openedai-speech's XTTS path intermittently 500s (a masked
        # `generator_worker` error, usually under concurrency / memory
        # pressure). One retry recovers it so the voice message still forms.
        url = f"{self.base_url}/v1/audio/speech"
        audio = b""
        for attempt in range(2):
            last = attempt == 1
            try:
                resp = await http.post(url, **kwargs)
            except (httpx.TransportError, httpx.TimeoutException) as e:
                if last:
                    raise
                log.warning("TTS request error (%s) — retrying once", e)
                await asyncio.sleep(1.0)
                continue
            if resp.status_code >= 500 and not last:
                log.warning("TTS backend HTTP %s — retrying once", resp.status_code)
                await asyncio.sleep(1.0)
                continue
            resp.raise_for_status()
            audio = resp.content
            break
        if self.denoise:
            audio = await _ffmpeg_denoise(audio, response_format, self.denoise)
        return audio
