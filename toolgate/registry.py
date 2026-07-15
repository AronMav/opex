"""Provider registry — resolves active providers by capability.

TTL=30s + ETag conditional GET (Task 18): `_refresh` caches the last fetch
timestamp + ETag header so steady-state traffic from many `aget_*` calls
collapses to (a) zero HTTP within the TTL window, and (b) a single 304-not-
modified hop once the TTL expires while Core's config hasn't changed.

On any fetch error (network failure, non-200/304 status, bad payload) we
keep the last-known config — readers never see an empty registry just because
Core blipped. Driver instances are rebuilt only when the fetched config
differs from the cached one (deep equality on the pydantic model).
"""

from __future__ import annotations

import asyncio
import logging
import os
import threading
import time

import httpx

from config import (
    CORE_API_URL,
    ProviderConfig,
    ProvidersConfig,
    _aload_config_from_api,
)
from providers.base import STTProvider, VisionProvider, TTSProvider, ImageGenProvider, EmbeddingProvider, WebSearchProvider

Provider = STTProvider | VisionProvider | TTSProvider | ImageGenProvider | EmbeddingProvider | WebSearchProvider

log = logging.getLogger("toolgate.registry")

# Lazy imports to avoid circular deps — populated in _build_driver_map()
_DRIVER_MAP: dict[tuple[str, str], type] | None = None


def _build_driver_map() -> dict[tuple[str, str], type]:
    # STT providers
    from providers.stt_local import LocalWhisperSTT
    from providers.stt_openai import OpenAISTT
    from providers.stt_groq import GroqSTT
    from providers.stt_deepgram import DeepgramSTT
    from providers.stt_google import GoogleSTT
    from providers.stt_mistral import MistralSTT
    from providers.stt_assemblyai import AssemblyAISTT
    from providers.stt_mimo import MiMoSTT
    from providers.stt_openrouter import OpenRouterSTT

    # Vision providers
    from providers.vision_local import OllamaVision
    from providers.vision_openai import OpenAIVision
    from providers.vision_google import GoogleVision
    from providers.vision_anthropic import AnthropicVision
    from providers.vision_replicate import ReplicateVision
    from providers.vision_qwen import QwenVision
    from providers.vision_cloudsight import CloudSightVision
    from providers.vision_mimo import MiMoVision

    # TTS providers
    from providers.tts_openai import OpenAITTS
    from providers.tts_elevenlabs import ElevenLabsTTS
    from providers.tts_edge import EdgeTTS
    from providers.tts_local import Qwen3TTS
    from providers.tts_fish_audio import FishAudioTTS
    from providers.tts_murf import MurfTTS
    from providers.tts_mimo import MiMoTTS
    from providers.tts_minimax import MiniMaxTTS
    from providers.tts_silero import SileroTTS

    # ImageGen providers
    from providers.imagegen_openai import OpenAIImageGen
    from providers.imagegen_runware import RunwareImageGen
    from providers.imagegen_stability import StabilityImageGen
    from providers.imagegen_fal import FalImageGen
    from providers.imagegen_pixazo import PixazoImageGen
    from providers.imagegen_comfyui import ComfyUIImageGen

    # Embedding providers
    from providers.embedding_ollama import OllamaEmbedding
    from providers.embedding_openai import OpenAIEmbedding

    # WebSearch providers
    from providers.websearch_searxng import SearxngWebSearch
    from providers.websearch_ollama import OllamaWebSearch
    from providers.websearch_brave import BraveWebSearch

    return {
        # STT
        ("stt", "whisper-local"): LocalWhisperSTT,
        ("stt", "openai"): OpenAISTT,
        ("stt", "groq"): GroqSTT,
        ("stt", "deepgram"): DeepgramSTT,
        ("stt", "google"): GoogleSTT,
        ("stt", "mistral"): MistralSTT,
        ("stt", "assemblyai"): AssemblyAISTT,
        ("stt", "mimo"): MiMoSTT,
        ("stt", "openrouter"): OpenRouterSTT,
        # Vision
        ("vision", "ollama"): OllamaVision,
        ("vision", "openai"): OpenAIVision,
        ("vision", "google"): GoogleVision,
        ("vision", "anthropic"): AnthropicVision,
        ("vision", "replicate"): ReplicateVision,
        ("vision", "qwen"): QwenVision,
        ("vision", "cloudsight"): CloudSightVision,
        ("vision", "mimo"): MiMoVision,
        # TTS
        ("tts", "openai"): OpenAITTS,
        ("tts", "elevenlabs"): ElevenLabsTTS,
        ("tts", "edge"): EdgeTTS,
        ("tts", "qwen3-tts"): Qwen3TTS,
        ("tts", "fish-audio"): FishAudioTTS,
        ("tts", "murf"): MurfTTS,
        ("tts", "mimo"): MiMoTTS,
        ("tts", "minimax"): MiniMaxTTS,
        ("tts", "silero"): SileroTTS,
        # ImageGen
        ("imagegen", "openai"): OpenAIImageGen,
        ("imagegen", "runware"): RunwareImageGen,
        ("imagegen", "stability"): StabilityImageGen,
        ("imagegen", "fal"): FalImageGen,
        ("imagegen", "pixazo"): PixazoImageGen,
        ("imagegen", "comfyui"): ComfyUIImageGen,
        # Embedding
        ("embedding", "ollama"): OllamaEmbedding,
        ("embedding", "openai"): OpenAIEmbedding,
        # WebSearch
        ("websearch", "searxng"): SearxngWebSearch,
        ("websearch", "ollama"): OllamaWebSearch,
        ("websearch", "brave"): BraveWebSearch,
    }


def get_driver_map() -> dict[tuple[str, str], type]:
    global _DRIVER_MAP
    if _DRIVER_MAP is None:
        _DRIVER_MAP = _build_driver_map()
    return _DRIVER_MAP


# Provider-based capabilities served by toolgate.
# Driver metadata (label, requires_key, list of drivers per capability) lives in
# config/media-drivers.yaml — that file is the single source of truth, served to
# the admin UI by Core via GET /api/media-drivers. Toolgate only needs the
# capability names internally; do NOT mirror driver metadata here.
CAPABILITIES = ["stt", "vision", "tts", "imagegen", "embedding", "websearch"]

# Utility services (no provider abstraction, always available)
UTILITY_SERVICES = [
    {"id": "documents", "endpoint": "/extract-text-url", "label": "Documents", "sub": "Text Extraction"},
    {"id": "fetch", "endpoint": "/fetch", "label": "Fetch", "sub": "URL Content"},
]


class ProviderRegistry:
    def __init__(self) -> None:
        self.config: ProvidersConfig = ProvidersConfig()
        self._instances: dict[str, Provider] = {}
        self._lock = threading.Lock()
        # ETag-cache state (Task 18): TTL window + last-seen ETag for conditional
        # GET. `_refresh_lock` serialises concurrent _refresh() calls so a single
        # client.get() actually fires per window even under high `aget_*` fan-in.
        self._etag: str | None = None
        self._last_fetch: float = 0.0
        self._refresh_lock = asyncio.Lock()

    async def _refresh(self) -> None:
        """TTL=30s + conditional GET. On error — keep last-known config.

        Steady state with healthy Core: ~one HTTP RTT per 30s, and that hop
        is usually a 304-not-modified (no body). Cold start (`self.config`
        empty) bypasses TTL so we keep retrying until Core answers.
        """
        async with self._refresh_lock:
            now = time.monotonic()
            # TTL hit — but only once we actually have providers. On cold start
            # we keep polling Core every call until something lands, otherwise
            # the registry would stay in degraded mode for the first 30s after
            # boot regardless of how fast Core comes up.
            if self.config.providers and (now - self._last_fetch) < 30:
                return

            token = os.environ.get("OPEX_AUTH_TOKEN") or os.environ.get("AUTH_TOKEN", "")
            headers: dict[str, str] = {}
            if token:
                headers["Authorization"] = f"Bearer {token}"
            if self._etag:
                headers["If-None-Match"] = self._etag

            try:
                async with httpx.AsyncClient() as client:
                    resp = await client.get(
                        f"{CORE_API_URL}/api/media-config",
                        headers=headers,
                        timeout=5.0,
                    )
            except Exception as e:
                log.warning("Core API fetch failed: %s — keep cached", e)
                return

            if resp.status_code == 304:
                # Config unchanged — refresh TTL but keep instances & ETag.
                self._last_fetch = now
                return

            if resp.status_code == 200:
                new_etag = resp.headers.get("ETag")
                try:
                    config = ProvidersConfig(**resp.json())
                except Exception:
                    log.exception("invalid media-config payload — keep cached")
                    return
                with self._lock:
                    if config != self.config:
                        self.config = config
                        self._instantiate_all()
                    self._etag = new_etag
                    self._last_fetch = now
                return

            log.warning("Core API returned %d — keep cached", resp.status_code)

    async def aload(self) -> None:
        """Startup warm-up — same as `_refresh` now.
        Retained for backwards compatibility with `app.py` lifespan."""
        await self._refresh()

    async def aget_active(self, capability: str) -> Provider | None:
        await self._refresh()
        with self._lock:
            active_id = self.config.active.get(capability)
            if active_id and active_id in self._instances:
                return self._instances[active_id]
            # profiles-мир: active-строк больше нет (кроме embedding) —
            # берём первый enabled-провайдер той же категории (id-порядок).
            for pid in sorted(self._instances):
                pcfg = self.config.providers.get(pid)
                if pcfg is not None and pcfg.type == capability:
                    return self._instances[pid]
            return None

    async def aget_instance(self, provider_id: str) -> Provider | None:
        await self._refresh()
        with self._lock:
            return self._instances.get(provider_id)

    def _instantiate_all(self) -> None:
        self._instances.clear()
        dm = get_driver_map()
        for pid, pcfg in self.config.providers.items():
            if not pcfg.enabled:
                continue
            key = (pcfg.type, pcfg.driver)
            cls = dm.get(key)
            if cls is None:
                log.warning("Unknown driver %s for provider %s", key, pid)
                continue
            try:
                self._instances[pid] = cls(
                    base_url=pcfg.base_url,
                    api_key=pcfg.api_key,
                    model=pcfg.model,
                    options=pcfg.options,
                )
            except Exception:
                log.exception("Failed to instantiate provider %s", pid)

    def list_providers(self) -> dict[str, ProviderConfig]:
        with self._lock:
            return self.config.providers

    def is_degraded(self) -> bool:
        """True iff the last successful load produced zero providers.
        When degraded, capability endpoints should return 503."""
        with self._lock:
            return not self.config.providers
