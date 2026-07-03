import pytest
import its.creds as creds

class FakeResp:
    def __init__(self, code, payload=None): self.status_code = code; self._p = payload
    def json(self): return self._p

class FakeHTTP:
    def __init__(self, resp): self._resp = resp; self.calls = 0
    async def get(self, url, headers=None, timeout=None):
        self.calls += 1; return self._resp

@pytest.mark.asyncio
async def test_get_credentials_caches(monkeypatch):
    creds._CACHE = None
    http = FakeHTTP(FakeResp(200, {"login": "u", "password": "p"}))
    a = await creds.get_credentials(http)
    b = await creds.get_credentials(http)
    assert a == {"login": "u", "password": "p"}
    assert b == a
    assert http.calls == 1   # второй раз из кэша

@pytest.mark.asyncio
async def test_get_credentials_none_on_404():
    creds._CACHE = None
    http = FakeHTTP(FakeResp(404))
    assert await creds.get_credentials(http) is None
