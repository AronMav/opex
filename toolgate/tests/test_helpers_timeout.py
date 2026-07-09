"""Regression test for F004 — download_limited must apply a default read timeout.

Without a read deadline (the shared client is built with read=None), a
slow-trickle origin holds an outbound connection open forever and, at ~20
concurrent such requests, exhausts the httpx connection pool — DoS-ing the
whole media hub. download_limited must set a default `timeout` (read=30s)
unless the caller passes its own. If a future author drops it, these go red.
"""

import httpx
import pytest

import helpers


class _FakeResp:
    headers = {"content-type": "text/plain"}

    def raise_for_status(self):
        pass

    async def aiter_bytes(self, _n):
        for chunk in (b"ok",):
            yield chunk


class _FakeStreamCtx:
    def __init__(self, rec, kwargs):
        self.rec, self.kwargs = rec, kwargs

    async def __aenter__(self):
        self.rec.update(self.kwargs)
        return _FakeResp()

    async def __aexit__(self, *_a):
        return False


class _FakeClient:
    def __init__(self, rec):
        self.rec = rec

    def stream(self, _method, _url, **kwargs):
        return _FakeStreamCtx(self.rec, kwargs)


@pytest.mark.asyncio
async def test_download_limited_sets_default_read_timeout(monkeypatch):
    monkeypatch.setattr(helpers, "validate_url_ssrf", lambda url: None)
    rec: dict = {}
    data, ct = await helpers.download_limited(_FakeClient(rec), "https://example.com/x")
    assert data == b"ok"
    timeout = rec.get("timeout")
    assert isinstance(timeout, httpx.Timeout)
    assert timeout.read == 30.0  # slow-loris guard (F004)


@pytest.mark.asyncio
async def test_download_limited_respects_caller_timeout(monkeypatch):
    monkeypatch.setattr(helpers, "validate_url_ssrf", lambda url: None)
    rec: dict = {}
    custom = httpx.Timeout(5.0)
    await helpers.download_limited(_FakeClient(rec), "https://example.com/x", timeout=custom)
    assert rec.get("timeout") is custom
