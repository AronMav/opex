"""Tests for Toolgate auth middleware hardening.

Covers:
- SEC-01: rightmost XFF from trusted proxy is used as real client IP
- SEC-02: TRUSTED_PROXIES="" disables XFF reading entirely
- SEC-03: forged XFF from untrusted sender is rejected
- Crash-safe parsing of INTERNAL_NETWORK and TRUSTED_PROXIES
"""

import importlib
import ipaddress
import os
import sys
from unittest.mock import MagicMock, patch

import pytest
from fastapi import Request
from fastapi.testclient import TestClient

# ── helpers ───────────────────────────────────────────────────────────────────

def _make_mod(internal_network="127.0.0.0/8", trusted_proxies="", auth_token="secret"):
    """Import (or re-import) app.py with the given env vars.

    Each call re-executes the module so that module-level constants (AUTH_TOKEN,
    _internal_nets, _trusted_proxies) are re-evaluated with the desired env.
    """
    env_patch = {
        "AUTH_TOKEN": auth_token,
        "INTERNAL_NETWORK": internal_network,
        "TRUSTED_PROXIES": trusted_proxies,
    }
    with patch.dict(os.environ, env_patch, clear=False):
        # Force re-import so module-level code runs again with new env
        if "app" in sys.modules:
            del sys.modules["app"]
        import app as toolgate_app
        importlib.reload(toolgate_app)
        return toolgate_app


def _mock_request(client_host: str | None, xff: str | None = None) -> MagicMock:
    """Build a minimal MagicMock that looks like a Starlette Request."""
    req = MagicMock(spec=Request)
    if client_host is None:
        req.client = None
    else:
        req.client = MagicMock()
        req.client.host = client_host
    # Simulate headers as a dict-like object
    headers: dict[str, str] = {}
    if xff is not None:
        headers["x-forwarded-for"] = xff
    req.headers = headers
    return req


def _auth_decision(mod, client_ip: str, auth_token_in_request: str | None = None) -> bool:
    """Return True if auth_middleware would PASS the request (not 401).

    This directly tests the auth decision without any HTTP overhead.
    Simulates: _is_internal(real_ip) where real_ip is the resolved client IP.
    """
    is_internal = mod._is_internal(client_ip)
    if not mod.AUTH_TOKEN:
        return True  # auth disabled
    if is_internal:
        return True
    if auth_token_in_request == mod.AUTH_TOKEN:
        return True
    return False


# ── tests ─────────────────────────────────────────────────────────────────────

class TestHealthPublicPath:
    def test_health_no_auth_needed(self):
        """GET /health always returns 200 without any token."""
        mod = _make_mod(auth_token="secret")
        with TestClient(mod.app, raise_server_exceptions=False) as client:
            response = client.get("/health")
        assert response.status_code == 200


class TestInternalBypass:
    def test_internal_localhost_no_auth(self):
        """Request from loopback → is_internal → passes without token."""
        mod = _make_mod(internal_network="127.0.0.0/8", trusted_proxies="", auth_token="secret")
        assert _auth_decision(mod, "127.0.0.1") is True

    def test_external_ip_requires_auth(self):
        """Request from non-internal IP without token → rejected."""
        mod = _make_mod(internal_network="127.0.0.0/8", trusted_proxies="", auth_token="secret")
        assert _auth_decision(mod, "8.8.8.8") is False

    def test_external_ip_with_valid_token(self):
        """Request from non-internal IP with valid token → passes."""
        mod = _make_mod(internal_network="127.0.0.0/8", trusted_proxies="", auth_token="secret")
        assert _auth_decision(mod, "8.8.8.8", auth_token_in_request="secret") is True


class TestForgedXFF:
    def test_forged_xff_from_untrusted_gets_401(self):
        """SEC-03: TRUSTED_PROXIES='', XFF='127.0.0.1', sender is non-internal → 401.

        _get_real_client_ip must return the sender's IP (ignoring XFF) when
        no trusted proxies are configured, so forged XFF cannot bypass auth.
        """
        mod = _make_mod(internal_network="127.0.0.0/8", trusted_proxies="", auth_token="secret")

        # Sender is 10.0.0.5 (not internal), XFF claims 127.0.0.1 (would be internal)
        req = _mock_request("10.0.0.5", xff="127.0.0.1")
        result = mod._get_real_client_ip(req)
        # Must return sender IP, not the forged XFF value
        assert result == "10.0.0.5", f"Expected sender IP '10.0.0.5', got '{result}'"
        # And the sender IP is not internal → auth would be required → 401
        assert not mod._is_internal(result)

    def test_empty_trusted_proxies_ignores_xff(self):
        """SEC-02: TRUSTED_PROXIES='' → XFF header completely ignored."""
        mod = _make_mod(trusted_proxies="", auth_token="secret")
        result = mod._get_real_client_ip(_mock_request("203.0.113.5", xff="127.0.0.1, 10.0.0.1"))
        assert result == "203.0.113.5"


class TestTrustedProxyXFF:
    def test_trusted_proxy_xff_rightmost(self):
        """SEC-01: sender in TRUSTED_PROXIES, XFF='192.168.1.5, 127.0.0.1' → uses 192.168.1.5."""
        mod = _make_mod(internal_network="127.0.0.0/8", trusted_proxies="127.0.0.0/8", auth_token="secret")
        result = mod._get_real_client_ip(_mock_request("127.0.0.1", xff="192.168.1.5, 127.0.0.1"))
        assert result == "192.168.1.5"

    def test_trusted_proxy_xff_internal_client(self):
        """Trusted proxy with XFF '127.0.0.2' → real IP is 127.0.0.2 → internal → 200."""
        mod = _make_mod(internal_network="127.0.0.0/8", trusted_proxies="127.0.0.0/8", auth_token="secret")
        result = mod._get_real_client_ip(_mock_request("127.0.0.1", xff="127.0.0.2, 127.0.0.1"))
        assert result == "127.0.0.2"

    def test_trusted_proxy_cidr_range(self):
        """TRUSTED_PROXIES='172.16.0.0/12,10.0.0.0/8' parses correctly."""
        mod = _make_mod(trusted_proxies="172.16.0.0/12,10.0.0.0/8", auth_token="secret")
        result = mod._get_real_client_ip(_mock_request("10.0.0.1", xff="8.8.8.8"))
        assert result == "8.8.8.8"

    def test_trusted_proxy_empty_xff(self):
        """Trusted proxy sends empty XFF → falls back to sender IP."""
        mod = _make_mod(trusted_proxies="127.0.0.0/8", auth_token="secret")
        result = mod._get_real_client_ip(_mock_request("127.0.0.1", xff=""))
        assert result == "127.0.0.1"

    def test_multiple_xff_entries_rightmost_wins(self):
        """XFF 'attacker_forged, real_client, proxy1' with only proxy1 trusted → real_client."""
        mod = _make_mod(trusted_proxies="10.0.0.0/8", auth_token="secret")
        # Walking from right: 10.0.0.1 is trusted, 5.6.7.8 is NOT trusted → return 5.6.7.8
        result = mod._get_real_client_ip(_mock_request("10.0.0.1", xff="1.2.3.4, 5.6.7.8, 10.0.0.1"))
        assert result == "5.6.7.8"


class TestGetRealClientIpUnit:
    def test_get_real_client_ip_unit_no_client(self):
        """request.client is None → returns '' (external)."""
        mod = _make_mod(trusted_proxies="", auth_token="secret")
        result = mod._get_real_client_ip(_mock_request(None))
        assert result == ""

    def test_get_real_client_ip_no_trusted_proxies(self):
        """No trusted proxies → always return sender IP."""
        mod = _make_mod(trusted_proxies="", auth_token="secret")
        result = mod._get_real_client_ip(_mock_request("203.0.113.1", xff="127.0.0.1"))
        assert result == "203.0.113.1"


class TestCrashSafeParsing:
    def test_invalid_internal_network_fallback(self):
        """INTERNAL_NETWORK='garbage' → falls back to 127.0.0.0/8, does not crash."""
        mod = _make_mod(internal_network="garbage", auth_token="")
        # If we get here without exception, the test passes
        # Verify fallback: 127.0.0.1 should still be internal
        import ipaddress
        localhost = ipaddress.ip_address("127.0.0.1")
        assert any(localhost in net for net in mod._internal_nets)

    def test_invalid_trusted_proxies_fallback(self):
        """TRUSTED_PROXIES='garbage' → falls back to empty list, does not crash."""
        mod = _make_mod(trusted_proxies="garbage", auth_token="")
        # If we get here without exception, the test passes
        assert mod._trusted_proxies == []
