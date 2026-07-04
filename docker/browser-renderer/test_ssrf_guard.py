import socket

import pytest

from ssrf_guard import is_private_or_metadata


@pytest.mark.parametrize("url", [
    "http://169.254.169.254/latest/meta-data/",
    "http://10.0.0.1/",
    "http://172.16.5.5/",
    "http://192.168.1.1:8080/",
    "http://127.0.0.1/",
    "http://[::1]/",
    "http://[fe80::1]/",
    "http://[fc00::1]/",
    "http://100.64.0.1/",  # CGNAT
    "http://0.0.0.0/",
])
def test_private_and_link_local_blocked(url):
    assert is_private_or_metadata(url) is True


def test_metadata_hostname_blocked_via_dns_mock(monkeypatch):
    def fake_getaddrinfo(host, *a, **kw):
        assert host == "metadata.google.internal"
        return [(socket.AF_INET, None, None, "", ("169.254.169.254", 0))]
    monkeypatch.setattr(socket, "getaddrinfo", fake_getaddrinfo)
    assert is_private_or_metadata("http://metadata.google.internal/computeMetadata/v1/") is True


def test_metadata_hostname_blocked_even_without_dns():
    # Belt-and-suspenders: literal known metadata hostname is blocked even if
    # DNS resolution were somehow unavailable, because we special-case the
    # hostname string itself before ever calling getaddrinfo.
    assert is_private_or_metadata("http://metadata.google.internal/") is True


@pytest.mark.parametrize("url", [
    "http://8.8.8.8/",
    "https://example.com/",
    "https://its.1c.ru/db/v854doc",
])
def test_public_addresses_allowed(url, monkeypatch):
    def fake_getaddrinfo(host, *a, **kw):
        return [(socket.AF_INET, None, None, "", ("93.184.216.34", 0))]
    monkeypatch.setattr(socket, "getaddrinfo", fake_getaddrinfo)
    assert is_private_or_metadata(url) is False


def test_unresolvable_hostname_fails_open(monkeypatch):
    def fake_getaddrinfo(host, *a, **kw):
        raise socket.gaierror("Name or service not known")
    monkeypatch.setattr(socket, "getaddrinfo", fake_getaddrinfo)
    assert is_private_or_metadata("http://this-does-not-resolve.invalid/") is False


@pytest.mark.parametrize("url", [None, "", "about:blank", "data:text/html,hi"])
def test_no_host_urls_allowed(url):
    assert is_private_or_metadata(url) is False


def test_dns_rebind_partial_private_hit_blocked(monkeypatch):
    """If ANY resolved address is private, block — mirrors the Rust
    SsrfSafeResolver's conservative filter behaviour."""
    def fake_getaddrinfo(host, *a, **kw):
        return [
            (socket.AF_INET, None, None, "", ("8.8.8.8", 0)),
            (socket.AF_INET, None, None, "", ("169.254.169.254", 0)),
        ]
    monkeypatch.setattr(socket, "getaddrinfo", fake_getaddrinfo)
    assert is_private_or_metadata("http://sneaky.example/") is True
