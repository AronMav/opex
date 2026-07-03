import pytest
from its.driver import BrowserDriver

class FakeResp:
    def __init__(self, payload): self._p = payload
    def raise_for_status(self): pass
    def json(self): return self._p

class FakeHTTP:
    def __init__(self): self.calls = []
    async def post(self, url, json=None, timeout=None):
        self.calls.append(json)
        action = json["action"]
        if action == "create_session": return FakeResp({"session_id": "abc", "status": "created"})
        if action == "content": return FakeResp({"html": "<b>hi</b>", "text": "hi", "url": "u"})
        return FakeResp({"status": "ok"})

@pytest.mark.asyncio
async def test_ensure_session_creates_once_with_profile():
    http = FakeHTTP()
    d = BrowserDriver(http)
    sid1 = await d.ensure_session()
    sid2 = await d.ensure_session()
    assert sid1 == sid2 == "abc"
    creates = [c for c in http.calls if c["action"] == "create_session"]
    assert len(creates) == 1
    assert creates[0]["profile"] == "its"

@pytest.mark.asyncio
async def test_navigate_passes_session_id():
    http = FakeHTTP()
    d = BrowserDriver(http)
    await d.navigate("https://its.1c.ru/db/")
    nav = [c for c in http.calls if c["action"] == "navigate"][0]
    assert nav["url"] == "https://its.1c.ru/db/"
    assert nav["session_id"] == "abc"
