"""Shared helpers for toolgate routers."""

import ipaddress
import re
import socket
from urllib.parse import urlparse

import httpx
from fastapi import HTTPException

MAX_DOWNLOAD_BYTES = 50 * 1024 * 1024  # 50 MB

# Per-download timeout. read=30s aborts a slow-trickle origin (slow-loris)
# that would otherwise hold an outbound connection open forever and exhaust
# the shared 20-connection pool, DoS-ing the whole hub (F004). Steady
# downloads are unaffected — the read timeout is per-chunk idle time, not
# total duration. Callers may override with their own `timeout=` kwarg.
DOWNLOAD_TIMEOUT = httpx.Timeout(connect=10.0, read=30.0, write=10.0, pool=10.0)

# CGNAT / carrier-grade NAT range (RFC 6598). Python's ipaddress stdlib
# does NOT classify this as private/reserved; mirror Rust
# crates/opex-core/src/net/ssrf.rs::is_private_ip.
_CGNAT_V4 = ipaddress.IPv4Network("100.64.0.0/10")


def validate_url_ssrf(url: str) -> None:
    """Block requests to private/internal networks (SSRF protection).

    Validates both the hostname (blocklist) and resolved IPs (private range check).
    Mirrors the Rust SsrfSafeResolver logic from the Core.
    """
    parsed = urlparse(url)
    scheme = parsed.scheme.lower()
    hostname = parsed.hostname or ""

    if scheme not in ("http", "https"):
        raise HTTPException(400, f"blocked: unsupported scheme '{scheme}'")

    # Hostname blocklist
    blocked_hosts = {"localhost", "127.0.0.1", "::1", "0.0.0.0",
                     "metadata.google.internal", "metadata.aws.internal"}
    if hostname in blocked_hosts or hostname.endswith(".local") or hostname.endswith(".internal"):
        raise HTTPException(400, f"blocked: URL targets internal service ({hostname})")

    # Resolve and check for private IPs
    try:
        for info in socket.getaddrinfo(hostname, None, socket.AF_UNSPEC, socket.SOCK_STREAM):
            addr = info[4][0]
            ip = ipaddress.ip_address(addr)
            if ip.is_private or ip.is_loopback or ip.is_link_local or ip.is_reserved:
                raise HTTPException(400, f"blocked: URL resolves to private IP ({addr})")
            # CGNAT 100.64.0.0/10 — stdlib does not flag this range; mirror Rust SoT.
            if isinstance(ip, ipaddress.IPv4Address) and ip in _CGNAT_V4:
                raise HTTPException(400, f"blocked: URL resolves to CGNAT IP ({addr})")
            # Multicast — IPv4 224.0.0.0/4 and IPv6 ff00::/8. Stdlib is_multicast covers both.
            if ip.is_multicast:
                raise HTTPException(400, f"blocked: URL resolves to multicast IP ({addr})")
    except socket.gaierror:
        pass  # DNS resolution will fail later in httpx — let it


async def download_limited(http, url: str, *, max_bytes: int = MAX_DOWNLOAD_BYTES, **kwargs):
    """Download URL with a size limit to prevent OOM. Returns (bytes, content_type)."""
    validate_url_ssrf(url)
    # Apply a default read timeout unless the caller set one (F004 slow-loris).
    kwargs.setdefault("timeout", DOWNLOAD_TIMEOUT)
    # follow_redirects=False mirrors Rust ssrf_http_client (commit 75fee11):
    # a 302 from a public origin could otherwise bypass the pre-flight
    # validate_url_ssrf check and land on a private-IP target.
    async with http.stream("GET", url, follow_redirects=False, **kwargs) as resp:
        resp.raise_for_status()
        cl = resp.headers.get("content-length")
        if cl and int(cl) > max_bytes:
            raise HTTPException(413, f"Response too large: {int(cl)} bytes (limit {max_bytes // 1048576}MB)")
        chunks = []
        total = 0
        async for chunk in resp.aiter_bytes(8192):
            total += len(chunk)
            if total > max_bytes:
                raise HTTPException(413, f"Response exceeds {max_bytes // 1048576}MB limit")
            chunks.append(chunk)
    return b"".join(chunks), resp.headers.get("content-type", "")

LANGUAGE_NAMES = {
    "ru": "Russian", "en": "English", "es": "Spanish", "de": "German",
    "fr": "French", "zh": "Chinese", "ja": "Japanese", "ko": "Korean",
    "pt": "Portuguese", "it": "Italian", "ar": "Arabic", "hi": "Hindi",
}


def default_vision_prompt(language: str) -> str:
    lang_name = LANGUAGE_NAMES.get(language, "English")
    return (
        "Describe this image in detail. Include: main subject, setting/background, "
        "colors, mood/atmosphere, any text or signs visible, notable details. "
        "If there are people — describe their appearance, clothing, expression, pose. "
        "If it's a screenshot — describe the UI, content, and any visible text. "
        "If it's a document or chart — extract key information. "
        f"Be thorough but concise. Respond in {lang_name}."
    )


def detect_image_type(data: bytes) -> str | None:
    if data[:3] == b'\xff\xd8\xff':
        return "image/jpeg"
    if data[:8] == b'\x89PNG\r\n\x1a\n':
        return "image/png"
    if data[:4] == b'GIF8':
        return "image/gif"
    if data[:4] == b'RIFF' and data[8:12] == b'WEBP':
        return "image/webp"
    if data[:4] == b'\x00\x00\x01\x00':
        return "image/x-icon"
    return None


def resolve_content_type(image_bytes: bytes, http_content_type: str = "") -> str:
    ct = detect_image_type(image_bytes)
    if ct:
        return ct
    ct = http_content_type.split(";")[0].strip() if http_content_type else ""
    if "image" in ct:
        return ct
    return "image/jpeg"


def check_upload_size(data: bytes, max_bytes: int, label: str = "File"):
    """Return a 413 JSONResponse if data exceeds max_bytes, else None."""
    if len(data) > max_bytes:
        from fastapi.responses import JSONResponse
        return JSONResponse(
            status_code=413,
            content={"error": f"{label} too large ({len(data)} bytes). Max {max_bytes // 1048576} MB."},
        )
    return None


def log_provider(log, provider):
    """Log the active provider name and model."""
    log.info("Using provider: %s model=%s", provider.name, getattr(provider, "model", ""))


def clean_html(text: str) -> str:
    """Strip script/style tags and HTML markup from text."""
    text = re.sub(r"<(script|style)[^>]*>.*?</\1>", "", text, flags=re.DOTALL | re.IGNORECASE)
    text = re.sub(r"<[^>]+>", " ", text)
    text = re.sub(r"\s{2,}", "\n", text).strip()
    return text
