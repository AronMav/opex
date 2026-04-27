"""Toolgate — Universal tool processing hub.

Supports multiple providers for STT, Vision, TTS, and Image Generation.
Utility services: document text extraction, URL content fetching.
Configuration loaded from Core API at startup.
"""

import logging
import os
import secrets
import time
from contextlib import asynccontextmanager

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse
import httpx

log = logging.getLogger("toolgate")

from registry import ProviderRegistry, CAPABILITIES

registry = ProviderRegistry()
http_client: httpx.AsyncClient = None


@asynccontextmanager
async def lifespan(app: FastAPI):
    global http_client
    # Phase 62 RES-07: cap outbound provider connections to prevent pool exhaustion
    # on Raspberry Pi. httpx queues requests past max_connections; PoolTimeout fires
    # after timeout.pool (120s here) — see Pitfall 4 in 62-RESEARCH.md.
    http_client = httpx.AsyncClient(
        timeout=120.0,
        limits=httpx.Limits(
            max_connections=20,
            max_keepalive_connections=10,
        ),
    )
    app.state.registry = registry
    app.state.http_client = http_client
    await registry.aload()
    yield
    if http_client:
        await http_client.aclose()


app = FastAPI(lifespan=lifespan)

from dependencies import _DegradedResponse, degraded_response


@app.exception_handler(_DegradedResponse)
async def _degraded_exception_handler(_request, exc: _DegradedResponse):
    return degraded_response(exc)


AUTH_TOKEN = os.environ.get("AUTH_TOKEN", "")
INTERNAL_NETWORK = os.environ.get("INTERNAL_NETWORK", "127.0.0.0/8")
TRUSTED_PROXIES = os.environ.get("TRUSTED_PROXIES", "")
# Paths/prefixes that don't require authentication
PUBLIC_PATHS = {"/health"}

import ipaddress


def _parse_networks(
    raw: str,
    fallback: list[str] | None = None,
) -> list[ipaddress.IPv4Network | ipaddress.IPv6Network]:
    """Parse comma-separated CIDR list. Returns fallback on any parse error."""
    if not raw.strip():
        return []
    try:
        return [ipaddress.ip_network(n.strip(), strict=False) for n in raw.split(",") if n.strip()]
    except ValueError as e:
        fb = fallback or []
        log.warning("Failed to parse network config %r: %s — using fallback %s", raw, e, fb)
        return [ipaddress.ip_network(f) for f in fb]


_internal_nets = _parse_networks(INTERNAL_NETWORK, fallback=["127.0.0.0/8"])
_trusted_proxies = _parse_networks(TRUSTED_PROXIES, fallback=[])


def _is_internal(client_host: str) -> bool:
    """Check if request comes from internal/trusted network."""
    try:
        addr = ipaddress.ip_address(client_host)
        return any(addr in net for net in _internal_nets)
    except ValueError:
        return False


def _get_real_client_ip(request: Request) -> str:
    """Return the real client IP, considering trusted proxy headers.

    Algorithm:
    1. Get sender_ip from request.client.host (TCP-level source).
    2. If sender_ip is NOT in _trusted_proxies, return sender_ip (ignore XFF).
    3. If sender_ip IS in _trusted_proxies, read X-Forwarded-For header.
    4. Walk XFF entries from RIGHT to LEFT. Return the first entry that is
       NOT in _trusted_proxies. This is the real client.
    5. If all entries are trusted proxies (shouldn't happen), return leftmost.
    """
    if not request.client:
        log.warning("request.client is None — treating as external")
        return ""

    sender_ip = request.client.host

    # If no trusted proxies configured, never read XFF (SEC-02)
    if not _trusted_proxies:
        return sender_ip

    # Check if the direct sender is a trusted proxy
    try:
        sender_addr = ipaddress.ip_address(sender_ip)
    except ValueError:
        return sender_ip

    is_trusted_sender = any(sender_addr in net for net in _trusted_proxies)
    if not is_trusted_sender:
        # Untrusted sender — ignore any XFF they sent (SEC-03)
        return sender_ip

    # Sender is trusted proxy — read XFF
    xff = request.headers.get("x-forwarded-for", "")
    if not xff:
        return sender_ip

    # Walk from right to left, find first non-proxy IP (SEC-01)
    parts = [p.strip() for p in xff.split(",") if p.strip()]
    for entry in reversed(parts):
        try:
            entry_addr = ipaddress.ip_address(entry)
            if not any(entry_addr in net for net in _trusted_proxies):
                return entry
        except ValueError:
            # Malformed entry — skip
            continue

    # All entries are proxies — use leftmost as best guess
    return parts[0] if parts else sender_ip


@app.middleware("http")
async def auth_middleware(request: Request, call_next):
    path = request.url.path
    if AUTH_TOKEN and path not in PUBLIC_PATHS:
        # Skip auth for inter-container traffic on Docker network
        real_ip = _get_real_client_ip(request)
        if _is_internal(real_ip):
            return await call_next(request)
        auth = request.headers.get("authorization", "")
        expected = f"Bearer {AUTH_TOKEN}"
        if not auth or not secrets.compare_digest(auth, expected):
            return JSONResponse(status_code=401, content={"error": "unauthorized"})
    return await call_next(request)


@app.middleware("http")
async def log_requests(request: Request, call_next):
    start = time.monotonic()
    response = await call_next(request)
    elapsed_ms = (time.monotonic() - start) * 1000
    log.info("%s %s → %d (%.0fms)", request.method, request.url.path, response.status_code, elapsed_ms)
    return response


# Mount routers
from routers import stt, vision, tts, imagegen, embedding, documents, fetch
from primitives import imap, smtp, google_calendar, bcs
app.include_router(stt.router)
app.include_router(vision.router)
app.include_router(tts.router)
app.include_router(imagegen.router)
app.include_router(embedding.router)
app.include_router(documents.router)
app.include_router(fetch.router)
app.include_router(imap.router)
app.include_router(smtp.router)
app.include_router(google_calendar.router)
app.include_router(bcs.router)


@app.get("/health")
async def health():
    active = {}
    available = {}
    for cap in CAPABILITIES:
        p = registry.get_active(cap)
        active[cap] = p.name if p else None
        available[cap] = p is not None
    degraded = registry.is_degraded()
    return {
        "status": "degraded" if degraded else "ok",
        "degraded": degraded,
        "loaded_providers": len(registry.list_providers()),
        "capabilities": available,
        "active_providers": active,
    }


@app.post("/reload")
async def reload_providers():
    """Reload provider configuration and invalidate router caches."""
    await registry.areload()
    # Invalidate cached credentials in workspace routers
    _invalidate_router_caches()
    active = {}
    for cap in CAPABILITIES:
        p = registry.get_active(cap)
        active[cap] = p.name if p else None
    log.info("Provider config reloaded via /reload endpoint")
    return {"ok": True, "active_providers": active}


def _invalidate_router_caches():
    """Clear cached tokens/state in primitives that read secrets."""
    try:
        from primitives.bcs import invalidate_cache
        invalidate_cache()
    except (ImportError, AttributeError):
        pass
