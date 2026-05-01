"""Base protocols for toolgate providers."""

from typing import Protocol, runtime_checkable

import httpx


def resolve_request_timeout(options: dict | None, default: float | None = None) -> float | None:
    """Read `options.timeouts.request_secs` (UI-configured per-provider override)
    or return `default`. Used by every media provider to honor the timeouts the
    operator sets in the Providers UI without each driver duplicating parsing
    logic. Returns float seconds or None when no override exists.

    Pass the result as `timeout=` to `httpx.AsyncClient` calls. When None,
    the call inherits the shared client default (toolgate sets 120s)."""
    if not options:
        return default
    timeouts = options.get("timeouts")
    if isinstance(timeouts, dict):
        v = timeouts.get("request_secs")
        if v is not None:
            try:
                return float(v)
            except (TypeError, ValueError):
                pass
    return default


@runtime_checkable
class STTProvider(Protocol):
    name: str

    async def transcribe(
        self,
        http: httpx.AsyncClient,
        audio_bytes: bytes,
        filename: str,
        language: str,
        model: str | None = None,
    ) -> str: ...


@runtime_checkable
class VisionProvider(Protocol):
    name: str

    async def describe(
        self,
        http: httpx.AsyncClient,
        image_bytes: bytes,
        content_type: str,
        prompt: str,
        max_tokens: int = 2000,
    ) -> str: ...


@runtime_checkable
class TTSProvider(Protocol):
    name: str

    async def synthesize(
        self,
        http: httpx.AsyncClient,
        text: str,
        voice: str,
        model: str | None = None,
        response_format: str = "mp3",
        registry=None,
    ) -> bytes: ...


@runtime_checkable
class ImageGenProvider(Protocol):
    name: str

    async def generate(
        self,
        http: httpx.AsyncClient,
        prompt: str,
        size: str = "1024x1024",
        model: str | None = None,
        quality: str = "standard",
    ) -> bytes: ...


@runtime_checkable
class EmbeddingProvider(Protocol):
    name: str

    async def embed(
        self,
        http: httpx.AsyncClient,
        texts: list[str],
        model: str | None = None,
    ) -> list[list[float]]: ...
