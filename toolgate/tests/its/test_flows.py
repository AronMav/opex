# toolgate/tests/its/test_flows.py
import pytest
from its.flows import ItsFlows, ItsBusy

_LOGGED_IN = "<a href='/login/?action=logout&provider=login'>Выйти</a>"
_LOGGED_OUT = "<a href='/user/auth'>Вход</a>"


class FakeDriver:
    def __init__(self, content_seq):
        # each content() call consumes the next blob (last one repeats)
        self._content = list(content_seq)
        self.filled = {}; self.clicked = []; self.navigated = []

    async def navigate(self, url, timeout=30): self.navigated.append(url); return {}
    async def fill(self, sel, val): self.filled[sel] = val
    async def click(self, sel): self.clicked.append(sel)
    async def wait(self, sel, timeout=10): return {}
    async def current_url(self): return "u"
    async def content(self):
        c = self._content.pop(0) if len(self._content) > 1 else self._content[0]
        return {"html": c, "text": "", "url": "u"}
    async def reset_session(self): pass


CFG = {  # минимальный SITE_ITS для теста
    "base_url": "https://its.1c.ru", "auth_probe_url": "https://its.1c.ru/",
    "logged_out": {"logged_in_marker": "action=logout"},
    "login": {"auth_page": "https://its.1c.ru/user/auth", "portal_selector": "#login_portal",
              "username_selector": "input[name=username]", "password_selector": "input[name=password]",
              "submit_selector": "input[name=submit]", "success_marker": "action=logout"},
    "relogin_cooldown_s": 300,
}


@pytest.mark.asyncio
async def test_login_performed_when_logged_out():
    # 1-я проба контента → разлогинен; после логина → маркер выхода присутствует
    drv = FakeDriver(content_seq=[_LOGGED_OUT, _LOGGED_IN])
    clock = {"t": 0.0}
    f = ItsFlows(drv, CFG, now_fn=lambda: clock["t"])
    await f.ensure_logged_in({"login": "u", "password": "p"})
    assert drv.filled["input[name=username]"] == "u"
    assert drv.filled["input[name=password]"] == "p"
    assert "#login_portal" in drv.clicked
    assert "input[name=submit]" in drv.clicked


@pytest.mark.asyncio
async def test_already_logged_in_skips_login():
    drv = FakeDriver(content_seq=[_LOGGED_IN])
    f = ItsFlows(drv, CFG, now_fn=lambda: 0.0)
    await f.ensure_logged_in({"login": "u", "password": "p"})
    assert drv.filled == {}   # логин не выполнялся
    assert drv.clicked == []


@pytest.mark.asyncio
async def test_relogin_cooldown_raises_busy():
    # разлогинен, но логинились только что (в пределах cooldown) → ItsBusy
    drv = FakeDriver(content_seq=[_LOGGED_OUT])
    clock = {"t": 100.0}
    f = ItsFlows(drv, CFG, now_fn=lambda: clock["t"])
    f._last_login_at = 99.0   # только что логинились
    with pytest.raises(ItsBusy):
        await f.ensure_logged_in({"login": "u", "password": "p"})
