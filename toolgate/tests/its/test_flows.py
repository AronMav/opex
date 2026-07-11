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
    async def frame_content(self, frame_sel, content_sel="body"):
        return {"html": "<div id='content'>тело</div>", "text": "", "url": "u"}
    async def reset_session(self): pass


CFG = {  # минимальный SITE_ITS для теста
    "base_url": "https://its.1c.ru", "auth_probe_url": "https://its.1c.ru/",
    "logged_out": {"logged_in_marker": "action=logout"},
    "login": {"auth_page": "https://its.1c.ru/user/auth", "portal_selector": "#login_portal",
              "username_selector": "input[name=username]", "password_selector": "input[name=password]",
              "submit_selector": "input[name=submit]", "success_marker": "action=logout"},
    "relogin_cooldown_s": 300,
    "read": {
        "print_url_template": None,
        "full_url_template": "{base}{ref}",
        "doc_frame_selector": "#w_metadata_doc_frame",
        "content_selector": "#content",
        "strip_selectors": ["nav"],
        "wait_selector": "#content",
    },
}


async def _nosleep(_s):
    pass


@pytest.mark.asyncio
async def test_read_relative_ref_prepends_base():
    drv = FakeDriver(content_seq=[_LOGGED_IN])
    f = ItsFlows(drv, CFG, now_fn=lambda: 0.0, sleep_fn=_nosleep)
    await f.read("/db/taxnds/content/2/hdoc")
    assert drv.navigated == ["https://its.1c.ru/db/taxnds/content/2/hdoc"]


@pytest.mark.asyncio
async def test_read_absolute_ref_is_not_doubled():
    # Регрессия: описание тула приглашает передавать «полный URL» в ref. Раньше
    # read делал {base}{ref} безусловно → https://its.1c.ruhttps://its.1c.ru/... →
    # мусорный хост → ERR_TUNNEL_CONNECTION_FAILED → 502. Абсолютный ref = как есть.
    drv = FakeDriver(content_seq=[_LOGGED_IN])
    f = ItsFlows(drv, CFG, now_fn=lambda: 0.0, sleep_fn=_nosleep)
    await f.read("https://its.1c.ru/db/taxnds/content/2/hdoc")
    assert drv.navigated == ["https://its.1c.ru/db/taxnds/content/2/hdoc"]


@pytest.mark.asyncio
async def test_login_performed_when_logged_out():
    # все пробы контента → разлогинен; после логина → маркер выхода присутствует
    drv = FakeDriver(content_seq=[_LOGGED_OUT] * 4 + [_LOGGED_IN])
    clock = {"t": 0.0}
    f = ItsFlows(drv, CFG, now_fn=lambda: clock["t"], sleep_fn=_nosleep)
    await f.ensure_logged_in({"login": "u", "password": "p"})
    assert drv.filled["input[name=username]"] == "u"
    assert drv.filled["input[name=password]"] == "p"
    assert "#login_portal" in drv.clicked
    assert "input[name=submit]" in drv.clicked


@pytest.mark.asyncio
async def test_already_logged_in_skips_login():
    drv = FakeDriver(content_seq=[_LOGGED_IN])
    f = ItsFlows(drv, CFG, now_fn=lambda: 0.0, sleep_fn=_nosleep)
    await f.ensure_logged_in({"login": "u", "password": "p"})
    assert drv.filled == {}   # логин не выполнялся
    assert drv.clicked == []


@pytest.mark.asyncio
async def test_slow_render_does_not_trigger_relogin():
    # Регрессия: на холодном браузере шапка с logout-ссылкой дорисовывается JS
    # после domcontentloaded — первый снимок DOM без маркера ещё не разлогин.
    # Ложный перелогин при живой сессии кончается ERR_ABORTED на /user/auth.
    drv = FakeDriver(content_seq=[_LOGGED_OUT, _LOGGED_OUT, _LOGGED_IN])
    f = ItsFlows(drv, CFG, now_fn=lambda: 0.0, sleep_fn=_nosleep)
    await f.ensure_logged_in({"login": "u", "password": "p"})
    assert drv.filled == {}   # логин не выполнялся
    assert drv.clicked == []


@pytest.mark.asyncio
async def test_relogin_cooldown_raises_busy():
    # разлогинен, но логинились только что (в пределах cooldown) → ItsBusy
    drv = FakeDriver(content_seq=[_LOGGED_OUT])
    clock = {"t": 100.0}
    f = ItsFlows(drv, CFG, now_fn=lambda: clock["t"], sleep_fn=_nosleep)
    f._last_login_at = 99.0   # только что логинились
    with pytest.raises(ItsBusy):
        await f.ensure_logged_in({"login": "u", "password": "p"})
