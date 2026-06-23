"""Helpers for workspace routers — simplifies common operations."""
import os
import httpx

# Shared client — reused across all requests (avoids TCP+TLS per call)
_client = httpx.AsyncClient(timeout=15)


def _core_url() -> str:
    """Resolve Core API URL at call time.

    Prefer CORE_API_URL (set by Core's managed_process env_extra), fall back
    to legacy CORE_URL, then localhost default. Read per-call rather than at
    import time so tests that monkeypatch env vars after import work.
    """
    return (
        os.environ.get("CORE_API_URL")
        or os.environ.get("CORE_URL", "http://127.0.0.1:18789")
    )


def _headers() -> dict:
    """Auth headers for core API (needed when core runs on host, not in Docker)."""
    token = os.environ.get("AUTH_TOKEN", "")
    if token:
        return {"Authorization": f"Bearer {token}"}
    return {}


async def get_secret(name: str, scope: str = "") -> str:
    """Read a secret from OPEX vault by name.

    Usage in a workspace router:
        from workspace_helpers import get_secret
        token = await get_secret("MY_API_KEY")
    """
    params = f"?reveal=true&scope={scope}" if scope else "?reveal=true"
    resp = await _client.get(f"{_core_url()}/api/secrets/{name}{params}", headers=_headers())
    if resp.status_code == 200:
        return resp.json().get("value", "")
    return ""


async def core_api(method: str, path: str, json: dict | None = None) -> dict:
    """Call OPEX core API.

    Usage: data = await core_api("GET", "/api/agents")
    """
    resp = await _client.request(method, f"{_core_url()}{path}", headers=_headers(), json=json)
    resp.raise_for_status()
    return resp.json()
