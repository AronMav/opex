"""SSRF-enforcing forward proxy for the headless browser.

Per-URL validation (`ssrf_guard.is_private_or_metadata`) cannot close the
DNS-rebinding TOCTOU: it resolves the host, but Chromium then re-resolves at
connect time, so an attacker who controls the DNS can return a public IP to the
check and a private IP (127.0.0.1 / 169.254.169.254 / RFC1918) to the browser.

This proxy closes it. Chromium is launched with `--proxy-server`, so EVERY
connection it makes — the initial navigation, HTTP redirects, sub-resources, and
JS `fetch()` — passes through here. For each, we resolve the host ONCE and open
the socket to that exact resolved IP (atomic resolve+connect, no rebinding
window), refusing any private/loopback/link-local/CGNAT/multicast/metadata
address. HTTPS `CONNECT` is tunneled to the pinned IP, so TLS SNI + certificate
verification still use the real hostname (we never put the IP in the URL).
"""

from __future__ import annotations

import asyncio
import ipaddress
import socket

from ssrf_guard import METADATA_HOSTNAMES, _ip_is_private_or_metadata

PROXY_HOST = "127.0.0.1"
PROXY_PORT = 3128


async def _resolve_safe(host: str, port: int) -> str | None:
    """Resolve `host` and return the first PUBLIC IP string, or None if the host
    is a known metadata name, resolves only to private/blocked addresses, or does
    not resolve at all. This single resolution is what the socket connects to —
    there is no second lookup for an attacker to rebind."""
    host = host.strip("[]").lower()
    if host in METADATA_HOSTNAMES:
        return None
    # Literal IP → classify directly.
    try:
        ip = ipaddress.ip_address(host)
        return None if _ip_is_private_or_metadata(ip) else str(ip)
    except ValueError:
        pass
    try:
        loop = asyncio.get_running_loop()
        infos = await loop.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    except (socket.gaierror, OSError, UnicodeError):
        return None
    for info in infos:
        addr = info[4][0]
        try:
            ip = ipaddress.ip_address(addr)
        except ValueError:
            continue
        if not _ip_is_private_or_metadata(ip):
            return addr
    return None


async def _pipe(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    try:
        while True:
            data = await reader.read(65536)
            if not data:
                break
            writer.write(data)
            await writer.drain()
    except (ConnectionError, asyncio.CancelledError, OSError):
        pass
    finally:
        try:
            writer.close()
        except OSError:
            pass


async def _deny(writer: asyncio.StreamWriter, status: bytes) -> None:
    try:
        writer.write(b"HTTP/1.1 " + status + b"\r\n\r\n")
        await writer.drain()
    except OSError:
        pass
    finally:
        try:
            writer.close()
        except OSError:
            pass


async def _handle(client_reader: asyncio.StreamReader, client_writer: asyncio.StreamWriter) -> None:
    try:
        head = await asyncio.wait_for(client_reader.readuntil(b"\r\n\r\n"), timeout=15)
    except (asyncio.IncompleteReadError, asyncio.LimitOverrunError, asyncio.TimeoutError):
        client_writer.close()
        return

    first = head.split(b"\r\n", 1)[0].decode("latin-1", "replace")
    parts = first.split()
    if len(parts) < 2:
        client_writer.close()
        return
    method, target = parts[0].upper(), parts[1]

    if method == "CONNECT":
        host, _, port_s = target.rpartition(":")
        port = int(port_s) if port_s.isdigit() else 443
        ip = await _resolve_safe(host, port)
        if ip is None:
            await _deny(client_writer, b"403 Forbidden")
            return
        try:
            remote_reader, remote_writer = await asyncio.open_connection(ip, port)
        except OSError:
            await _deny(client_writer, b"502 Bad Gateway")
            return
        client_writer.write(b"HTTP/1.1 200 Connection established\r\n\r\n")
        await client_writer.drain()
        await asyncio.gather(
            _pipe(client_reader, remote_writer),
            _pipe(remote_reader, client_writer),
        )
        return

    # Plain HTTP: the request-line carries an absolute URI (proxy form).
    from urllib.parse import urlsplit

    u = urlsplit(target)
    host = u.hostname or ""
    port = u.port or 80
    if not host:
        client_writer.close()
        return
    ip = await _resolve_safe(host, port)
    if ip is None:
        await _deny(client_writer, b"403 Forbidden")
        return
    try:
        remote_reader, remote_writer = await asyncio.open_connection(ip, port)
    except OSError:
        await _deny(client_writer, b"502 Bad Gateway")
        return
    # Forward the request head verbatim. RFC 7230 §5.3.2 requires origin servers
    # to accept the absolute-form request-target, so no rewrite is needed; the
    # body (if any) is streamed by the client→remote pipe.
    remote_writer.write(head)
    await remote_writer.drain()
    await asyncio.gather(
        _pipe(client_reader, remote_writer),
        _pipe(remote_reader, client_writer),
    )


async def start_proxy() -> asyncio.AbstractServer:
    """Start the SSRF proxy on 127.0.0.1:PROXY_PORT and return the server."""
    return await asyncio.start_server(_handle, PROXY_HOST, PROXY_PORT)
