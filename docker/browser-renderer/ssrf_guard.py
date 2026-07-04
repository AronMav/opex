"""Private-network / cloud-metadata guard for browser-renderer.

browser-renderer (Playwright/Chromium) has no SSRF protection of its own.
The only pre-existing gate lives on the Rust side (`net/ssrf.rs`,
`comms.rs:BrowserActionHandler`) and only fires on the `url` argument of the
initial `navigate` / `create_session` call. Once the browser is live, a page
can end up on a private/metadata address via:

  * an HTTP redirect inside `page.goto` (T01 §2, T08 §4)
  * client-side JS navigation (`evaluate`, a clicked link)
  * `back` (`page.go_back()`, T08 §1)

...and every subsequent action (`click`/`type`/`fill`/`evaluate`/`content`/
`text`/`screenshot`/...) would run unguarded against whatever `page.url`
currently is. This module gives browser-renderer its own floor, independent
of the Rust-side pre-check, applied against the *current* `page.url` right
before dispatching any action.

Fail-open/closed policy (documented per T08 review request):

  * Any address that resolves (or is a literal IP) to a private/loopback/
    link-local/CGNAT/multicast/cloud-metadata range → BLOCKED (fail-closed).
    This includes hostnames like `metadata.google.internal` that resolve to
    the 169.254.169.254 metadata floor.
  * A hostname that fails to resolve at all (DNS error, timeout, NXDOMAIN)
    is treated as "not private" → allowed through (fail-open). Rationale:
    browser-renderer's job is general-purpose web browsing of arbitrary
    public sites; treating every transient DNS hiccup as a security block
    would make the tool unreliable and would not by itself grant access to
    anything sensitive — the resolver already failed to find a route to
    that host, so the immediate risk is unavailability, not exfiltration.
    The one asymmetry we care about (private ranges) is fail-closed by
    construction because we explicitly test *for* those ranges below.
"""

from __future__ import annotations

import ipaddress
import socket
from urllib.parse import urlsplit

# Cloud-metadata hostnames that don't literally look like an IP but resolve
# to (or otherwise stand in for) the well-known 169.254.169.254 floor. Kept
# as an explicit belt-and-suspenders list in case DNS resolution in the
# container is somehow unable to reach them (e.g. no route, sandboxed
# network) — those cases still get treated as private, not "unresolvable".
METADATA_HOSTNAMES = {
    "metadata.google.internal",
    "metadata.goog",
    "metadata",  # AWS IMDS convenience name inside some networks
}


def _ip_is_private_or_metadata(ip: ipaddress.IPv4Address | ipaddress.IPv6Address) -> bool:
    """Mirror of Rust `net/ssrf.rs::is_private_ip` (kept in sync by hand)."""
    if isinstance(ip, ipaddress.IPv4Address):
        if ip.is_loopback or ip.is_private or ip.is_link_local:
            return True
        if ip.is_multicast or ip.is_unspecified:
            return True
        if str(ip) == "255.255.255.255":
            return True
        octets = ip.packed
        # 100.64.0.0/10 — Carrier-grade NAT
        if octets[0] == 100 and (octets[1] & 0xC0) == 64:
            return True
        return False

    # IPv6
    mapped = ip.ipv4_mapped
    if mapped is not None:
        return _ip_is_private_or_metadata(ipaddress.IPv4Address(mapped))
    if ip.is_loopback or ip.is_unspecified or ip.is_multicast or ip.is_link_local:
        return True
    # RFC 4193 Unique Local fc00::/7
    if (ip.packed[0] & 0xFE) == 0xFC:
        return True
    # Teredo 2001:0000::/32
    if ip.packed[0:2] == b"\x20\x01" and ip.packed[2:4] == b"\x00\x00":
        return True
    # 6to4 2002::/16
    if ip.packed[0:2] == b"\x20\x02":
        return True
    return False


def _resolve_host(host: str) -> list[str] | None:
    """Best-effort DNS resolution. Returns None on failure (caller decides
    fail-open/closed), or a list of resolved IP strings on success."""
    try:
        infos = socket.getaddrinfo(host, None)
    except (socket.gaierror, OSError, UnicodeError):
        return None
    ips: list[str] = []
    for info in infos:
        sockaddr = info[4]
        if sockaddr:
            ips.append(sockaddr[0])
    return ips or None


def is_private_or_metadata(url: str | None) -> bool:
    """Return True if `url`'s host is a literal private/loopback/link-local/
    CGNAT/multicast IP, a known cloud-metadata hostname, or resolves (via
    DNS) to any such address.

    Fail-closed for anything that IS private/metadata; fail-open (returns
    False) for hostnames that simply don't resolve, or for URLs we can't
    parse a host out of at all (e.g. `about:blank`, `data:` URLs — these
    have no network-reachable host so there's nothing to protect against).
    """
    if not url:
        return False

    try:
        parsed = urlsplit(url)
    except ValueError:
        # Malformed URL string — no host to evaluate against.
        return False

    host = parsed.hostname
    if not host:
        # Schemes like about:, data:, blob:, chrome-error: etc. have no
        # network host at all.
        return False

    host_lower = host.lower()
    if host_lower in METADATA_HOSTNAMES:
        return True

    # Literal IP (v4 or v6, with or without brackets already stripped by
    # urlsplit for the .hostname accessor).
    try:
        ip = ipaddress.ip_address(host_lower)
        return _ip_is_private_or_metadata(ip)
    except ValueError:
        pass

    # DNS name: resolve and check every returned address (any private hit
    # blocks the whole result — consistent with the Rust SsrfSafeResolver's
    # "filter, and if empty result set, block" logic, but here even a
    # *partial* private hit is enough since we can't force Chromium to only
    # use the public answers post-hoc).
    resolved = _resolve_host(host_lower)
    if resolved is None:
        # Unresolvable — fail-open (see module docstring).
        return False

    for ip_str in resolved:
        try:
            ip = ipaddress.ip_address(ip_str)
        except ValueError:
            continue
        if _ip_is_private_or_metadata(ip):
            return True
    return False
