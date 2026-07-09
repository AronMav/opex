"""Browser Renderer — headless Chromium text extraction + automation service."""

import asyncio
import time
import uuid
from contextlib import asynccontextmanager

from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel, Field
from playwright.async_api import async_playwright, Browser, Page

from automation_actions import dispatch_action
from profiles import ProfileManager
from ssrf_guard import is_private_or_metadata
from ssrf_proxy import PROXY_HOST, PROXY_PORT, start_proxy
from stealth import STEALTH_INIT_JS, stealth_context_kwargs

# F051: route ALL Chromium traffic through the in-process SSRF proxy, which
# atomically resolves+pins each host to a validated public IP (closing the
# DNS-rebinding TOCTOU that per-URL checks cannot). `<-loopback>` forces even
# loopback/localhost navigations through the proxy so they are blocked too.
_PROXY_ARGS = [
    f"--proxy-server=http://{PROXY_HOST}:{PROXY_PORT}",
    "--proxy-bypass-list=<-loopback>",
]


def _ssrf_check(url: str | None) -> None:
    """Block private/loopback/link-local/CGNAT/metadata destinations. The
    /extract and /screenshot endpoints previously did a bare `page.goto` with NO
    SSRF check of their own — only toolgate's pre-flight validated, which a
    redirect inside goto could escape (and a direct call to this internal service
    would bypass entirely). Mirror the /automation guard: check BEFORE goto
    (reject an obviously-private target without fetching) and AFTER goto (catch a
    302/JS redirect to a private address before returning its content)."""
    if is_private_or_metadata(url):
        raise HTTPException(status_code=400, detail=f"blocked: URL targets a private/internal address ({url})")

browser: Browser | None = None
pw_instance = None
profile_manager: ProfileManager | None = None
ssrf_proxy_server = None

# ── Session management ────────────────────────────────────────────────────────
sessions: dict[str, Page] = {}
session_last_used: dict[str, float] = {}
session_dialog: dict[str, dict] = {}
# Sids backed by a named persistent profile (see ProfileManager). These are
# exempt from the ephemeral idle TTL — a profile-backed page must survive
# gaps between workflow steps (e.g. assisted-login pauses).
persistent_sessions: set[str] = set()
SESSION_TTL = 300  # 5 minutes idle timeout
CLEANUP_INTERVAL = 30  # seconds


def _expired_sids(now: float) -> list[str]:
    """Pure(ish) helper: which sids are idle past SESSION_TTL, excluding
    profile-backed (persistent) sessions which never expire on idle."""
    return [
        sid for sid, last in session_last_used.items()
        if sid not in persistent_sessions and now - last > SESSION_TTL
    ]


async def session_cleanup_task():
    """Background task to close idle sessions."""
    while True:
        await asyncio.sleep(CLEANUP_INTERVAL)
        now = time.time()
        expired = _expired_sids(now)
        for sid in expired:
            page = sessions.pop(sid, None)
            session_last_used.pop(sid, None)
            if page:
                try:
                    await page.close()
                except Exception:
                    pass


def touch_session(sid: str):
    session_last_used[sid] = time.time()


def get_session_page(session_id: str) -> Page:
    if session_id not in sessions:
        raise HTTPException(404, f"Session {session_id} not found")
    touch_session(session_id)
    return sessions[session_id]


def _make_dialog_handler(sid: str):
    """JS-dialog handler: by default accept dialogs (recording the message) so
    automation never hangs. `set_dialog` can switch to dismiss / set prompt text."""
    async def _handler(dialog):
        st = session_dialog.setdefault(sid, {"accept": True, "prompt_text": None, "last": None})
        st["last"] = dialog.message
        try:
            if st.get("accept", True):
                await dialog.accept(st.get("prompt_text") or "")
            else:
                await dialog.dismiss()
        except Exception:
            pass
    return _handler


@asynccontextmanager
async def lifespan(app: FastAPI):
    global browser, pw_instance, ssrf_proxy_server
    # Start the SSRF proxy BEFORE the browser so every launched Chromium can
    # reach it. If it fails to bind, the browser can't navigate → the container
    # goes unhealthy (fail-closed), which is the intended safety posture.
    ssrf_proxy_server = await start_proxy()
    pw_instance = await async_playwright().start()
    browser = await pw_instance.chromium.launch(
        headless=True,
        args=["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage", *_PROXY_ARGS],
    )

    global profile_manager
    async def _persistent_factory(user_data_dir: str):
        ctx = await pw_instance.chromium.launch_persistent_context(
            user_data_dir=user_data_dir,
            headless=True,
            args=["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
                  "--disable-blink-features=AutomationControlled", *_PROXY_ARGS],
            **stealth_context_kwargs(),
        )
        await ctx.add_init_script(STEALTH_INIT_JS)
        return ctx
    profile_manager = ProfileManager(factory=_persistent_factory)

    cleanup = asyncio.create_task(session_cleanup_task())
    yield
    cleanup.cancel()
    # Close all sessions
    for page in sessions.values():
        try:
            await page.close()
        except Exception:
            pass
    sessions.clear()
    if profile_manager:
        await profile_manager.close_all()
    await browser.close()
    await pw_instance.stop()
    if ssrf_proxy_server is not None:
        ssrf_proxy_server.close()
        try:
            await ssrf_proxy_server.wait_closed()
        except Exception:
            pass


app = FastAPI(title="Browser Renderer", lifespan=lifespan)

# ── Original endpoints (stateless) ───────────────────────────────────────────

class ExtractRequest(BaseModel):
    url: str
    timeout: int = Field(default=30, ge=1, le=60, description="Page load timeout in seconds")
    selector: str | None = Field(default=None, description="CSS selector to wait for before extracting")


class ExtractResponse(BaseModel):
    title: str
    description: str
    text: str
    url: str


STRIP_SELECTORS = [
    "script", "style", "noscript", "iframe", "svg",
    "nav", "header", "footer", "[role=navigation]",
    "[role=banner]", "[class*=cookie]", "[class*=popup]",
    "[class*=modal]", "[class*=sidebar]", "[class*=ad-]",
    "[class*=advertisement]", "[id*=ad-]",
]

CONTENT_SELECTORS = ["article", "main", "[role=main]", ".content", "#content", "body"]

DEFAULT_USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
DEFAULT_VIEWPORT = {"width": 1280, "height": 720}


@app.post("/extract", response_model=ExtractResponse)
async def extract(req: ExtractRequest):
    _ssrf_check(req.url)
    page = await browser.new_page(
        user_agent=DEFAULT_USER_AGENT,
    )
    try:
        await page.goto(req.url, wait_until="domcontentloaded", timeout=req.timeout * 1000)
        _ssrf_check(page.url)  # a redirect inside goto may have landed on a private host

        # Wait for JS rendering: custom selector or a short delay
        if req.selector:
            try:
                await page.wait_for_selector(req.selector, timeout=10000)
            except Exception:
                pass
        else:
            await page.wait_for_timeout(3000)

        # Extract metadata
        title = await page.title() or ""
        description = await page.evaluate(
            """() => {
                const el = document.querySelector('meta[name="description"]');
                return el ? el.getAttribute('content') || '' : '';
            }"""
        )

        # Strip noise elements
        for sel in STRIP_SELECTORS:
            await page.evaluate(
                f"document.querySelectorAll('{sel}').forEach(el => el.remove())"
            )

        # Extract main content
        text = ""
        for sel in CONTENT_SELECTORS:
            result = await page.evaluate(
                f"""() => {{
                    const el = document.querySelector('{sel}');
                    return el ? el.innerText : '';
                }}"""
            )
            cleaned = " ".join(result.split()) if result else ""
            if len(cleaned) > 100:
                text = cleaned
                break

        # Truncate to ~8000 chars
        if len(text) > 8000:
            text = text[:8000] + "..."

        return ExtractResponse(
            title=title.strip(),
            description=(description or "").strip(),
            text=text,
            url=req.url,
        )
    finally:
        await page.close()


class ScreenshotRequest(BaseModel):
    url: str
    timeout: int = Field(default=15, ge=1, le=60)
    full_page: bool = False


@app.post("/screenshot")
async def screenshot(req: ScreenshotRequest):
    _ssrf_check(req.url)
    page = await browser.new_page(
        viewport=DEFAULT_VIEWPORT,
        user_agent=DEFAULT_USER_AGENT,
    )
    try:
        await page.goto(req.url, wait_until="domcontentloaded", timeout=req.timeout * 1000)
        _ssrf_check(page.url)  # a redirect inside goto may have landed on a private host
        await page.wait_for_timeout(2000)
        img_bytes = await page.screenshot(full_page=req.full_page)
        media_type = "image/png"
        if len(img_bytes) > 10 * 1024 * 1024:  # Telegram limit ~10MB
            # Re-take as JPEG with quality reduction for large screenshots
            img_bytes = await page.screenshot(full_page=req.full_page, type="jpeg", quality=80)
            media_type = "image/jpeg"
        return Response(content=img_bytes, media_type=media_type)
    finally:
        await page.close()


# ── Automation endpoints (stateful sessions) ─────────────────────────────────

class AutomationRequest(BaseModel):
    action: str
    session_id: str | None = None
    url: str | None = None
    selector: str | None = None
    text: str | None = None
    js: str | None = None
    timeout: int = Field(default=10, ge=1, le=60)
    fields: dict | None = None
    full_page: bool = False
    key: str | None = None
    dx: int | None = None
    dy: int | None = None
    to: str | None = None
    to_selector: str | None = None
    accept: bool | None = None
    prompt_text: str | None = None
    profile: str | None = None


@app.post("/automation")
async def automation(req: AutomationRequest):
    """Unified browser automation endpoint. Dispatches by `action` field."""
    action = req.action

    # ── create_session ────────────────────────────────────────────────────
    if action == "create_session":
        sid = str(uuid.uuid4())[:8]
        if req.profile:
            ctx = await profile_manager.get_context(req.profile)
            # New owner takes over the profile: close pages left by previous
            # clients (e.g. toolgate restarts). Persistent pages are exempt
            # from the idle TTL, so without this they accumulate until the
            # container mem_limit and page loads start timing out.
            for stale in list(ctx.pages):
                try:
                    await stale.close()
                except Exception:
                    pass
            for dead_sid in [s for s, p in sessions.items() if p.is_closed()]:
                sessions.pop(dead_sid, None)
                session_last_used.pop(dead_sid, None)
                session_dialog.pop(dead_sid, None)
                persistent_sessions.discard(dead_sid)
            page = await ctx.new_page()
            persistent_sessions.add(sid)
        else:
            page = await browser.new_page(
                viewport=DEFAULT_VIEWPORT, user_agent=DEFAULT_USER_AGENT,
            )
        sessions[sid] = page
        page.on("dialog", _make_dialog_handler(sid))
        session_dialog[sid] = {"accept": True, "prompt_text": None, "last": None}
        touch_session(sid)
        return {"session_id": sid, "status": "created", "profile": req.profile}

    # All other actions require session_id
    if not req.session_id:
        raise HTTPException(400, "session_id is required for this action")

    page = get_session_page(req.session_id)

    # ── close (local: pops session state) ─────────────────────────────────
    if action == "close":
        sessions.pop(req.session_id, None)
        session_last_used.pop(req.session_id, None)
        session_dialog.pop(req.session_id, None)
        persistent_sessions.discard(req.session_id)
        await page.close()
        return {"status": "closed", "session_id": req.session_id}

    # All other actions are handled by the testable dispatcher.
    return await dispatch_action(page, req, req.session_id, session_dialog)


@app.get("/health")
async def health():
    return {"status": "ok", "sessions": len(sessions)}
