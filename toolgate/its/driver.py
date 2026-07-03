"""Generic browser-renderer client bound to the 'its' persistent profile."""
import asyncio
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

    async def _eval(self, js: str, timeout: float = 30.0) -> dict:
        """evaluate with retry: a client-side redirect firing just after
        `domcontentloaded` destroys the JS execution context mid-evaluate
        ('Execution context was destroyed'). That's transient — settle briefly
        and retry instead of failing the whole ИТС request."""
        sid = await self.ensure_session()
        last = ""
        for _ in range(4):
            resp = await self._http.post(
                f"{self._url}/automation",
                json={"action": "evaluate", "session_id": sid, "js": js},
                timeout=timeout,
            )
            if resp.status_code == 200:
                return resp.json()
            last = resp.text
            if resp.status_code == 500 and "context was destroyed" in last:
                await asyncio.sleep(0.8)
                continue
            resp.raise_for_status()
        raise RuntimeError(f"evaluate failed after retries: {last[:200]}")

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
        # Full page HTML via evaluate: the browser-renderer 'content' action
        # truncates HTML at 50 KB, which drops ITS search results (the results
        # container sits ~60 KB into the DOM). evaluate returns its value
        # uncapped.
        r = await self._eval("({html: document.documentElement.outerHTML, url: location.href})")
        data = r.get("result") or {}
        return {"html": data.get("html", ""), "text": "", "url": data.get("url", "")}

    async def current_url(self) -> str:
        r = await self._eval("location.href")
        return r.get("result", "") or ""

    async def frame_content(self, frame_selector: str, ready_selector: str = "body") -> dict:
        """Return the outerHTML of a same-origin iframe's document, polling up
        to ~15s until `ready_selector` inside it has text. ИТС renders the
        article body in an iframe whose src just redirects back to the shell
        page, so we read its contentDocument in place. selector args come from
        trusted site config; repr() safely quotes them."""
        js = (
            "(async () => {"
            f"  const f = document.querySelector({frame_selector!r});"
            "  for (let i = 0; i < 30; i++) {"
            "    const d = f && f.contentDocument;"
            f"    const c = d && d.querySelector({ready_selector!r});"
            "    if (c && c.innerText.trim().length > 0)"
            "      return {html: d.documentElement.outerHTML, url: d.location.href};"
            "    await new Promise(r => setTimeout(r, 500));"
            "  }"
            "  const d = f && f.contentDocument;"
            "  return d ? {html: d.documentElement.outerHTML, url: d.location.href} : null;"
            "})()"
        )
        r = await self._eval(js, timeout=30)
        data = r.get("result") or {}
        return {"html": data.get("html", ""), "text": "", "url": data.get("url", "")}
