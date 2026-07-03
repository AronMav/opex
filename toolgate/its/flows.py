# toolgate/its/flows.py
"""ИТС login/search/read flows. Delegates browser work to BrowserDriver."""
import asyncio
import time
import urllib.parse

from .extract import extract_content, parse_search_results


class ItsBusy(Exception):
    """Сессия занята (вероятно, человеком) — консервативный перехват, S1."""


class ItsLoginFailed(Exception):
    """Логин не удался (креды/капча/2FA)."""


class ItsFlows:
    def __init__(self, driver, cfg: dict, now_fn=time.monotonic):
        self._d = driver
        self._cfg = cfg
        self._now = now_fn
        self._last_login_at = -1e9

    async def _is_logged_out(self) -> bool:
        url = await self._d.current_url()
        return self._cfg["logged_out"]["url_contains"] in url

    async def ensure_logged_in(self, creds: dict) -> None:
        await self._d.navigate(self._cfg["auth_probe_url"])
        if not await self._is_logged_out():
            return
        # Консервативный перехват: не логинимся чаще cooldown
        if self._now() - self._last_login_at < self._cfg["relogin_cooldown_s"]:
            raise ItsBusy("ИТС-сессия занята (вероятно, используется человеком); попробуйте позже")
        lc = self._cfg["login"]
        await self._d.fill(lc["login_selector"], creds["login"])
        await self._d.fill(lc["password_selector"], creds["password"])
        await self._d.click(lc["submit_selector"])
        self._last_login_at = self._now()
        await asyncio.sleep(1.0)  # человеческий темп + время редиректа
        url = await self._d.current_url()
        if lc["success_url_contains"] not in url:
            raise ItsLoginFailed(f"после логина остались на {url}")

    async def search(self, query: str, db: str | None = None) -> list[dict]:
        sc = self._cfg["search"]
        q = urllib.parse.quote(query)
        url = sc["url_template"].format(base=self._cfg["base_url"], q=q)
        if db and sc.get("db_scoped"):
            url += f"&db={urllib.parse.quote(db)}"
        await self._d.navigate(url)
        if sc.get("results_wait"):
            try:
                await self._d.wait(sc["results_wait"], timeout=15)
            except Exception:
                pass
        html = (await self._d.content())["html"]
        return parse_search_results(html, sc)

    async def read(self, ref: str) -> dict:
        rc = self._cfg["read"]
        if rc.get("print_url_template"):   # путь (a)
            url = rc["print_url_template"].format(base=self._cfg["base_url"], ref=ref)
        else:                              # путь (b)
            url = rc["full_url_template"].format(base=self._cfg["base_url"], ref=ref)
        await self._d.navigate(url)
        if rc.get("wait_selector"):
            try:
                await self._d.wait(rc["wait_selector"], timeout=15)
            except Exception:
                pass
        page = await self._d.content()
        out = extract_content(page["html"], rc["content_selector"], rc["strip_selectors"])
        out["url"] = page.get("url", url)
        return out
