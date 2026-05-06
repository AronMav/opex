"""Provider registry — resolves active providers by capability.

Pull-on-call (TTL=0): every `aget_active` / `aget_instance` fetches the latest
config from Core API. On fetch failure the registry keeps the last-known
config — readers never see an empty registry just because Core blipped.

Driver instances are rebuilt only when the fetched config differs from the
cached one (deep equality on the pydantic model), so the steady-state cost
of pull-on-call is one HTTP round-trip per call, not a full re-instantiation.
"""

from __future__ import annotations

import logging
import threading

from config import (
    ProviderConfig,
    ProvidersConfig,
    _aload_config_from_api,
)
from providers.base import STTProvider, VisionProvider, TTSProvider, ImageGenProvider, EmbeddingProvider

Provider = STTProvider | VisionProvider | TTSProvider | ImageGenProvider | EmbeddingProvider

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
    
    # Vision providers
    from providers.vision_local import OllamaVision
    from providers.vision_openai import OpenAIVision
    from providers.vision_google import GoogleVision
    from providers.vision_anthropic import AnthropicVision
    from providers.vision_replicate import ReplicateVision
    from providers.vision_qwen import QwenVision
    from providers.vision_cloudsight import CloudSightVision
    
    # TTS providers
    from providers.tts_openai import OpenAITTS
    from providers.tts_elevenlabs import ElevenLabsTTS
    from providers.tts_edge import EdgeTTS
    from providers.tts_local import Qwen3TTS
    from providers.tts_fish_audio import FishAudioTTS
    from providers.tts_murf import MurfTTS
    
    # ImageGen providers
    from providers.imagegen_openai import OpenAIImageGen
    from providers.imagegen_runware import RunwareImageGen
    from providers.imagegen_stability import StabilityImageGen
    from providers.imagegen_fal import FalImageGen
    from providers.imagegen_pixazo import PixazoImageGen

    # Embedding providers
    from providers.embedding_ollama import OllamaEmbedding
    from providers.embedding_openai import OpenAIEmbedding

    return {
        # STT
        ("stt", "whisper-local"): LocalWhisperSTT,
        ("stt", "openai"): OpenAISTT,
        ("stt", "groq"): GroqSTT,
        ("stt", "deepgram"): DeepgramSTT,
        ("stt", "google"): GoogleSTT,
        ("stt", "mistral"): MistralSTT,
        ("stt", "assemblyai"): AssemblyAISTT,
        # Vision
        ("vision", "ollama"): OllamaVision,
        ("vision", "openai"): OpenAIVision,
        ("vision", "google"): GoogleVision,
        ("vision", "anthropic"): AnthropicVision,
        ("vision", "replicate"): ReplicateVision,
        ("vision", "qwen"): QwenVision,
        ("vision", "cloudsight"): CloudSightVision,
        # TTS
        ("tts", "openai"): OpenAITTS,
        ("tts", "elevenlabs"): ElevenLabsTTS,
        ("tts", "edge"): EdgeTTS,
        ("tts", "qwen3-tts"): Qwen3TTS,
        ("tts", "fish-audio"): FishAudioTTS,
        ("tts", "murf"): MurfTTS,
        # ImageGen
        ("imagegen", "openai"): OpenAIImageGen,
        ("imagegen", "runware"): RunwareImageGen,
        ("imagegen", "stability"): StabilityImageGen,
        ("imagegen", "fal"): FalImageGen,
        ("imagegen", "pixazo"): PixazoImageGen,
        # Embedding
        ("embedding", "ollama"): OllamaEmbedding,
        ("embedding", "openai"): OpenAIEmbedding,
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
CAPABILITIES = ["stt", "vision", "tts", "imagegen", "embedding"]

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

    async def _refresh(self) -> None:
        """Pull-on-call: best-effort fetch; on failure, keep last-known.
        Rebuilds driver instances only when the fetched config differs."""
        config = await _aload_config_from_api()
        if config is None:
            return  # Core unreachable — keep cached
        with self._lock:
            if config == self.config:
                return  # No change — skip rebuild
            self.config = config
            self._instantiate_all()

    async def aload(self) -> None:
        """Startup warm-up — same as `_refresh` now.
        Retained for backwards compatibility with `app.py` lifespan."""
        await self._refresh()

    async def aget_active(self, capability: str) -> Provider | None:
        await self._refresh()
        with self._lock:
            active_id = self.config.active.get(capability)
            return self._instances.get(active_id) if active_id else None

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
