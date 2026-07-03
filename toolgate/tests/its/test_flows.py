# toolgate/tests/its/test_flows.py
import pytest
from its.flows import ItsFlows, ItsBusy

class FakeDriver:
    def __init__(self, url_seq, content_html=""):
        self._urls = list(url_seq); self._content = content_html
        self.filled = {}; self.clicked = []; self.navigated = []
    async def navigate(self, url, timeout=30): self.navigated.append(url); return {}
    async def fill(self, sel, val): self.filled[sel] = val
    async def click(self, sel): self.clicked.append(sel)
    async def wait(self, sel, timeout=10): return {}
    async def current_url(self):
        return self._urls.pop(0) if len(self._urls) > 1 else self._urls[0]
    async def content(self): return {"html": self._content, "text": "", "url": "u"}
    async def reset_session(self): pass

CFG = {  # минимальный SITE_ITS для теста
    "base_url": "https://its.1c.ru", "auth_probe_url": "https://its.1c.ru/db/",
    "logged_out": {"url_contains": "login.1c.ru", "form_selector": "input#l"},
    "login": {"login_selector": "input#l", "password_selector": "input#p",
              "submit_selector": "button#s", "success_url_contains": "its.1c.ru",
              "kicked_selector": None},
    "relogin_cooldown_s": 300,
}

@pytest.mark.asyncio
async def test_login_performed_when_logged_out():
    # 1-й current_url → на login; после submit → its.1c.ru
    drv = FakeDriver(url_seq=["https://login.1c.ru/", "https://its.1c.ru/db/"])
    clock = {"t": 0.0}
    f = ItsFlows(drv, CFG, now_fn=lambda: clock["t"])
    await f.ensure_logged_in({"login": "u", "password": "p"})
    assert drv.filled["input#l"] == "u"
    assert drv.filled["input#p"] == "p"
    assert "button#s" in drv.clicked

@pytest.mark.asyncio
async def test_already_logged_in_skips_login():
    drv = FakeDriver(url_seq=["https://its.1c.ru/db/"])
    f = ItsFlows(drv, CFG, now_fn=lambda: 0.0)
    await f.ensure_logged_in({"login": "u", "password": "p"})
    assert drv.filled == {}   # логин не выполнялся

@pytest.mark.asyncio
async def test_relogin_cooldown_raises_busy():
    # всё время на login-странице (выкидывает), в пределах cooldown → ItsBusy
    drv = FakeDriver(url_seq=["https://login.1c.ru/"])
    clock = {"t": 100.0}
    f = ItsFlows(drv, CFG, now_fn=lambda: clock["t"])
    f._last_login_at = 99.0   # только что логинились
    with pytest.raises(ItsBusy):
        await f.ensure_logged_in({"login": "u", "password": "p"})
