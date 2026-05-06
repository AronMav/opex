"""Configuration management for toolgate providers."""

import logging
import os
import asyncio

import httpx
from pydantic import BaseModel, Field

CORE_API_URL = os.environ.get("CORE_API_URL", "http://127.0.0.1:18789")

_log = logging.getLogger("toolgate.config")


class ProviderConfig(BaseModel):
    type: str
    driver: str
    base_url: str = ""
    model: str | None = None
    api_key: str | None = None
    enabled: bool = True
    options: dict = Field(default_factory=dict)


class ProvidersConfig(BaseModel):
    version: int = 1
    active: dict[str, str | None] = Field(default_factory=dict)
    providers: dict[str, ProviderConfig] = Field(default_factory=dict)


async def _aload_config_from_api() -> ProvidersConfig | None:
    """Try to load config from Core API (GET /api/media-config) asynchronously.

    Returns the parsed ProvidersConfig on success, or None if unavailable.
    """
    core_url = os.environ.get("CORE_API_URL", CORE_API_URL)
    if not core_url:
        return None
    # Read token at call time (not import time)
    auth_token = os.environ.get("HYDECLAW_AUTH_TOKEN", os.environ.get("AUTH_TOKEN", ""))
    try:
        headers: dict[str, str] = {}
        if auth_token:
            headers["Authorization"] = f"Bearer {auth_token}"
        async with httpx.AsyncClient() as client:
            resp = await client.get(
                f"{core_url}/api/media-config",
                headers=headers,
                timeout=5.0,
            )
        if resp.status_code == 200:
            data = resp.json()
            config = ProvidersConfig(**data)
            _log.info(
                "Loaded config from Core API: %d providers, active=%s",
                len(config.providers),
                list(config.active.keys()),
            )
            return config
        else:
            _log.warning(
                "Core API /api/media-config returned status %d — will retry",
                resp.status_code,
            )
    except Exception as e:
        _log.warning("Failed to load config from Core API: %s — will retry", e)
    return None

async def aload_config() -> ProvidersConfig:
    """Load config from Core API with retry.
    Returns empty ProvidersConfig if Core is unreachable after all retries.
    No env fallback — Core API is the single source of truth."""
    for attempt in range(5):
        config = await _aload_config_from_api()
        if config is not None:
            return config
        if attempt < 4:
            wait = 2 * (attempt + 1)
            _log.info("Core API not ready, retrying in %ds (attempt %d/5)...", wait, attempt + 1)
            await asyncio.sleep(wait)

    _log.error(
        "Core API unavailable after 5 attempts — starting in DEGRADED mode (no providers). "
        "Capability endpoints will return 503 until Core becomes reachable."
    )
    return ProvidersConfig()
