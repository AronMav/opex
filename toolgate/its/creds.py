"""Fetch ИТС credentials from Core vault via internal endpoint. Cached."""
import os

CORE_API_URL = os.environ.get("CORE_API_URL", "http://127.0.0.1:18789")
_CACHE: dict | None = None


async def get_credentials(http) -> dict | None:
    global _CACHE
    if _CACHE is not None:
        return _CACHE
    token = os.environ.get("OPEX_AUTH_TOKEN", os.environ.get("AUTH_TOKEN", ""))
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    try:
        resp = await http.get(f"{CORE_API_URL}/api/internal/its-credentials",
                              headers=headers, timeout=5.0)
    except Exception:
        return None
    if resp.status_code != 200:
        return None
    data = resp.json()
    if not data.get("login") or not data.get("password"):
        return None
    _CACHE = {"login": data["login"], "password": data["password"]}
    return _CACHE
