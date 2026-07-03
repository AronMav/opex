"""Generic browser-renderer client bound to the 'its' persistent profile."""
import os

# toolgate runs as a HOST process (not inside the docker network), so the
# docker-internal name 'browser-renderer' does not resolve — reach the
# container through its published loopback port instead. Overridable via env
# for a containerised toolgate where the service name would resolve.
BROWSER_URL = os.environ.get("BROWSER_RENDERER_URL", "http://127.0.0.1:9020")
PROFILE = "its"


class BrowserDriver:
    def __init__(self, http, browser_url: str = BROWSER_URL):
        self._http = http
        self._url = browser_url
        self._sid: str | None = None

    async def _call(self, payload: dict, timeout: float = 30.0) -> dict:
        resp = await self._http.post(f"{self._url}/automation", json=payload, timeout=timeout)
        resp.raise_for_status()
        return resp.json()

    async def ensure_session(self) -> str:
        if self._sid:
            return self._sid
        r = await self._call({"action": "create_session", "profile": PROFILE})
        self._sid = r["session_id"]
        return self._sid

    async def reset_session(self) -> None:
        self._sid = None

    async def navigate(self, url: str, timeout: int = 30) -> dict:
        sid = await self.ensure_session()
        return await self._call(
            {"action": "navigate", "session_id": sid, "url": url, "timeout": timeout},
            timeout=timeout + 10)

    async def fill(self, selector: str, value: str) -> dict:
        sid = await self.ensure_session()
        return await self._call({"action": "type", "session_id": sid, "selector": selector, "text": value})

    async def click(self, selector: str) -> dict:
        sid = await self.ensure_session()
        return await self._call({"action": "click", "session_id": sid, "selector": selector})

    async def wait(self, selector: str, timeout: int = 10) -> dict:
        sid = await self.ensure_session()
        return await self._call(
            {"action": "wait", "session_id": sid, "selector": selector, "timeout": timeout},
            timeout=timeout + 10)

    async def content(self) -> dict:
        sid = await self.ensure_session()
        # Full page HTML via evaluate: the browser-renderer 'content' action
        # truncates HTML at 50 KB, which drops ITS search results (the results
        # container sits ~60 KB into the DOM). evaluate returns its value
        # uncapped.
        r = await self._call({
            "action": "evaluate",
            "session_id": sid,
            "js": "({html: document.documentElement.outerHTML, url: location.href})",
        })
        data = r.get("result") or {}
        return {"html": data.get("html", ""), "text": "", "url": data.get("url", "")}

    async def current_url(self) -> str:
        sid = await self.ensure_session()
        r = await self._call({"action": "evaluate", "session_id": sid, "js": "location.href"})
        return r.get("result", "") or ""

    async def get_attribute(self, selector: str, attr: str) -> str | None:
        sid = await self.ensure_session()
        # selector/attr come from trusted site config; repr() safely quotes them.
        js = (f"(function(){{var e=document.querySelector({selector!r});"
              f"return e?e.getAttribute({attr!r}):null;}})()")
        r = await self._call({"action": "evaluate", "session_id": sid, "js": js})
        return r.get("result")
