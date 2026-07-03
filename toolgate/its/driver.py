"""Generic browser-renderer client bound to the 'its' persistent profile."""

BROWSER_URL = "http://browser-renderer:9020"
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
        return await self._call({"action": "content", "session_id": sid})

    async def current_url(self) -> str:
        r = await self.content()
        return r.get("url", "")
