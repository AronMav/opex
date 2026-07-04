import importlib.util
import sys
import types
from types import SimpleNamespace

import pytest

# Stub playwright so importing app-side modules never needs a real browser —
# but only when playwright isn't actually installed. Clobbering a real
# `playwright.async_api` module in sys.modules here would break other test
# files in the same pytest session that need the genuine API (e.g. app.py's
# `from playwright.async_api import async_playwright, Browser, Page`).
if importlib.util.find_spec("playwright") is None:
    sys.modules.setdefault("playwright", types.ModuleType("playwright"))
    sys.modules.setdefault("playwright.async_api", types.ModuleType("playwright.async_api"))

from automation_actions import dispatch_action  # noqa: E402


class FakeEl:
    def __init__(self):
        self.scrolled = False

    async def scroll_into_view_if_needed(self):
        self.scrolled = True


class FakeKeyboard:
    def __init__(self, p):
        self.p = p

    async def press(self, key):
        self.p.calls.append(("kb_press", key))


class FakeMouse:
    def __init__(self, p):
        self.p = p

    async def wheel(self, dx, dy):
        self.p.calls.append(("wheel", dx, dy))


class FakePage:
    def __init__(self):
        self.calls = []
        self.url = "http://example.test/"
        self.keyboard = FakeKeyboard(self)
        self.mouse = FakeMouse(self)

    async def hover(self, sel, timeout=None):
        self.calls.append(("hover", sel))

    async def drag_and_drop(self, a, b, timeout=None):
        self.calls.append(("drag", a, b))

    async def go_back(self, **kw):
        self.calls.append(("back",))

    async def press(self, sel, key, timeout=None):
        self.calls.append(("press", sel, key))

    async def query_selector(self, sel):
        return FakeEl()

    async def evaluate(self, js):
        self.calls.append(("evaluate", js))
        return None


def req(**kw):
    base = dict(action=None, session_id="s1", url=None, selector=None, text=None,
                js=None, timeout=10, fields=None, full_page=False, key=None,
                dx=None, dy=None, to=None, to_selector=None, accept=None, prompt_text=None)
    base.update(kw)
    return SimpleNamespace(**base)


@pytest.mark.asyncio
async def test_hover():
    p = FakePage()
    await dispatch_action(p, req(action="hover", selector="#b"), "s1", {})
    assert ("hover", "#b") in p.calls


@pytest.mark.asyncio
async def test_drag():
    p = FakePage()
    await dispatch_action(p, req(action="drag", selector="#a", to_selector="#b"), "s1", {})
    assert ("drag", "#a", "#b") in p.calls


@pytest.mark.asyncio
async def test_back():
    p = FakePage()
    await dispatch_action(p, req(action="back"), "s1", {})
    assert ("back",) in p.calls


@pytest.mark.asyncio
async def test_press_with_and_without_selector():
    p = FakePage()
    await dispatch_action(p, req(action="press", selector="#i", key="Enter"), "s1", {})
    assert ("press", "#i", "Enter") in p.calls
    await dispatch_action(p, req(action="press", key="Escape"), "s1", {})
    assert ("kb_press", "Escape") in p.calls


@pytest.mark.asyncio
async def test_scroll_bottom_default():
    p = FakePage()
    await dispatch_action(p, req(action="scroll"), "s1", {})
    assert any(c[0] == "evaluate" and "scrollHeight" in c[1] for c in p.calls)


@pytest.mark.asyncio
async def test_set_dialog_updates_state():
    p = FakePage()
    store = {"s1": {"accept": True, "prompt_text": None, "last": "hi"}}
    out = await dispatch_action(p, req(action="set_dialog", accept=False, prompt_text="ok"), "s1", store)
    assert store["s1"]["accept"] is False
    assert store["s1"]["prompt_text"] == "ok"
    assert out["last_dialog"] == "hi"


@pytest.mark.asyncio
async def test_unknown_action_raises():
    from fastapi import HTTPException
    p = FakePage()
    with pytest.raises(HTTPException):
        await dispatch_action(p, req(action="bogus"), "s1", {})


# ── Post-navigation SSRF guard (T08 §1, §4) ─────────────────────────────────

@pytest.mark.asyncio
async def test_action_blocked_when_page_on_metadata_ip():
    from fastapi import HTTPException
    p = FakePage()
    p.url = "http://169.254.169.254/latest/meta-data/"
    with pytest.raises(HTTPException) as exc:
        await dispatch_action(p, req(action="click", selector="#x"), "s1", {})
    assert exc.value.status_code == 403


@pytest.mark.asyncio
async def test_content_blocked_when_page_on_private_ip():
    from fastapi import HTTPException
    p = FakePage()
    p.url = "http://10.0.0.5/"

    async def inner_text(_sel):
        return "should not be reached"
    p.inner_text = inner_text

    async def content():
        return "<html></html>"
    p.content = content

    with pytest.raises(HTTPException) as exc:
        await dispatch_action(p, req(action="content"), "s1", {})
    assert exc.value.status_code == 403


@pytest.mark.asyncio
async def test_evaluate_blocked_when_page_on_private_ip():
    from fastapi import HTTPException
    p = FakePage()
    p.url = "http://192.168.1.1/"
    with pytest.raises(HTTPException):
        await dispatch_action(p, req(action="evaluate", js="1+1"), "s1", {})
    # evaluate() itself was never invoked
    assert not p.calls


@pytest.mark.asyncio
async def test_action_allowed_on_public_page():
    p = FakePage()  # default url is http://example.test/
    out = await dispatch_action(p, req(action="hover", selector="#b"), "s1", {})
    assert out["status"] == "hovered"


@pytest.mark.asyncio
async def test_back_blocked_when_landing_on_private_ip():
    from fastapi import HTTPException

    class BackToPrivatePage(FakePage):
        async def go_back(self, **kw):
            self.calls.append(("back",))
            self.url = "http://169.254.169.254/"

    p = BackToPrivatePage()
    with pytest.raises(HTTPException) as exc:
        await dispatch_action(p, req(action="back"), "s1", {})
    assert exc.value.status_code == 403
    assert ("back",) in p.calls  # go_back() did run; only the post-check blocks


@pytest.mark.asyncio
async def test_back_allowed_when_landing_on_public_page():
    p = FakePage()

    async def go_back(**kw):
        p.calls.append(("back",))
        p.url = "http://example.test/other"
    p.go_back = go_back

    out = await dispatch_action(p, req(action="back"), "s1", {})
    assert out["status"] == "navigated_back"
    assert out["url"] == "http://example.test/other"


@pytest.mark.asyncio
async def test_navigate_blocked_when_goto_redirects_to_private():
    from fastapi import HTTPException

    class RedirectingPage(FakePage):
        async def goto(self, url, **kw):
            self.calls.append(("goto", url))
            self.url = "http://169.254.169.254/"

        async def title(self):
            return "redirected"

    p = RedirectingPage()
    with pytest.raises(HTTPException) as exc:
        await dispatch_action(p, req(action="navigate", url="http://example.test/redirect-me"), "s1", {})
    assert exc.value.status_code == 403


@pytest.mark.asyncio
async def test_navigate_allowed_when_goto_stays_public():
    class GotoPage(FakePage):
        async def goto(self, url, **kw):
            self.calls.append(("goto", url))
            self.url = url

        async def title(self):
            return "ok"

    p = GotoPage()
    out = await dispatch_action(p, req(action="navigate", url="http://example.test/"), "s1", {})
    assert out["status"] == "navigated"


@pytest.mark.asyncio
async def test_navigate_allowed_for_its_profile_public_domain():
    """Regression guard for the ITS integration (workspace/tools/its.yaml,
    profile='its'): its.1c.ru is a legitimate public domain and must never
    trip the private/metadata guard."""
    class ItsPage(FakePage):
        async def goto(self, url, **kw):
            self.calls.append(("goto", url))
            self.url = url

        async def title(self):
            return "ИТС"

    p = ItsPage()
    out = await dispatch_action(
        p, req(action="navigate", url="https://its.1c.ru/db/v854doc"), "s1", {}
    )
    assert out["status"] == "navigated"
