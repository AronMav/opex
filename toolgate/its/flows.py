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
    def __init__(self, driver, cfg: dict, now_fn=time.monotonic, sleep_fn=asyncio.sleep):
        self._d = driver
        self._cfg = cfg
        self._now = now_fn
        self._sleep = sleep_fn
        self._last_login_at = -1e9

    async def _is_logged_out(self) -> bool:
        # its.1c.ru never redirects anonymous hits to a login form — it just
        # drops the logout link from the header. Detect by page content: the
        # logged-in marker present == authenticated.
        # Шапка с logout-ссылкой дорисовывается JS ПОСЛЕ domcontentloaded, и на
        # холодном браузере одиночный снимок даёт ложный «разлогинен» (а логин
        # при живой сессии кончается net::ERR_ABORTED на /user/auth — она сразу
        # редиректит залогиненного назад). Поэтому поллим маркер до ~3с.
        marker = self._cfg["logged_out"]["logged_in_marker"]
        for attempt in range(4):
            page = await self._d.content()
            blob = (page.get("html") or "") + (page.get("text") or "")
            if marker in blob:
                return False
            if attempt < 3:
                await self._sleep(1.0)
        return True

    async def ensure_logged_in(self, creds: dict) -> None:
        await self._d.navigate(self._cfg["auth_probe_url"])
        if not await self._is_logged_out():
            return
        # Консервативный перехват: не логинимся чаще cooldown
        if self._now() - self._last_login_at < self._cfg["relogin_cooldown_s"]:
            raise ItsBusy("ИТС-сессия занята (вероятно, используется человеком); попробуйте позже")
        lc = self._cfg["login"]
        # Multi-step SSO: its.1c.ru/user/auth → portal button → login.1c.ru form.
        await self._d.navigate(lc["auth_page"])
        await self._d.click(lc["portal_selector"])
        await self._d.wait(lc["username_selector"], timeout=15)
        await self._d.fill(lc["username_selector"], creds["login"])
        await self._d.fill(lc["password_selector"], creds["password"])
        await self._d.click(lc["submit_selector"])
        self._last_login_at = self._now()
        await self._sleep(2.0)  # человеческий темп + редирект обратно на its.1c.ru
        # Проверяем маркер логина на реальной странице (URL не редиректит).
        await self._d.navigate(self._cfg["auth_probe_url"])
        if await self._is_logged_out():
            raise ItsLoginFailed("после логина остались разлогинены (креды/капча/2FA?)")

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
        ref = ref.strip()
        # ref может быть уже абсолютным URL — описание тула прямо приглашает
        # передавать «полный URL» (напр. поле url из прошлого чтения). Раньше
        # read безусловно клеил {base}{ref} → https://its.1c.ruhttps://its.1c.ru/...
        # → мусорный хост → net::ERR_TUNNEL_CONNECTION_FAILED → 500 → 502.
        # Абсолютный ref используем как есть; базу приклеиваем только к относительному.
        if ref.startswith(("http://", "https://")):
            url = ref
        elif rc.get("print_url_template"):   # путь (a)
            url = rc["print_url_template"].format(base=self._cfg["base_url"], ref=ref)
        else:                                # путь (b)
            url = rc["full_url_template"].format(base=self._cfg["base_url"], ref=ref)
        await self._d.navigate(url)
        # ИТС рендерит тело документа в same-origin iframe (его src редиректит
        # обратно на страницу-обёртку), поэтому читаем contentDocument на месте.
        frame_sel = rc.get("doc_frame_selector")
        if frame_sel:
            page = await self._d.frame_content(frame_sel, rc.get("content_selector", "body"))
            if not page.get("html"):
                page = await self._d.content()  # запасной путь, если iframe не отдал
        else:
            if rc.get("wait_selector"):
                try:
                    await self._d.wait(rc["wait_selector"], timeout=15)
                except Exception:
                    pass
            page = await self._d.content()
        out = extract_content(page["html"], rc["content_selector"], rc["strip_selectors"])
        out["url"] = page.get("url", url)
        return out
