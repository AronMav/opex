"""Browser automation action dispatch. No playwright import at module level so
this is unit-testable with a fake page object."""

from fastapi import HTTPException
from fastapi.responses import Response


async def dispatch_action(page, req, sid, session_dialog):
    """Handle every action except create_session. `page` is a Playwright Page (or a
    fake in tests); `session_dialog` is the per-session dialog-state dict."""
    action = req.action

    if action == "navigate":
        if not req.url:
            raise HTTPException(400, "url is required")
        await page.goto(req.url, wait_until="domcontentloaded", timeout=req.timeout * 1000)
        title = await page.title() if hasattr(page, "title") else ""
        return {"status": "navigated", "url": req.url, "title": title or ""}

    if action == "click":
        if not req.selector:
            raise HTTPException(400, "selector is required")
        await page.click(req.selector, timeout=req.timeout * 1000)
        return {"status": "clicked", "selector": req.selector}

    if action == "type":
        if not req.selector or req.text is None:
            raise HTTPException(400, "selector and text are required")
        await page.fill(req.selector, req.text)
        return {"status": "typed", "selector": req.selector}

    if action == "fill":
        if not req.fields:
            raise HTTPException(400, "fields dict is required")
        for sel, val in req.fields.items():
            await page.fill(sel, str(val))
        return {"status": "filled", "fields_count": len(req.fields)}

    if action == "screenshot":
        png_bytes = await page.screenshot(full_page=req.full_page)
        return Response(content=png_bytes, media_type="image/png")

    if action == "wait":
        if not req.selector:
            raise HTTPException(400, "selector is required")
        await page.wait_for_selector(req.selector, timeout=req.timeout * 1000)
        return {"status": "found", "selector": req.selector}

    if action == "text":
        if req.selector:
            el = await page.query_selector(req.selector)
            if not el:
                return {"text": "", "error": f"Selector '{req.selector}' not found"}
            text = await el.inner_text()
        else:
            text = await page.inner_text("body")
        if len(text) > 8000:
            text = text[:8000] + "..."
        return {"text": text}

    if action == "evaluate":
        if not req.js:
            raise HTTPException(400, "js is required")
        result = await page.evaluate(req.js)
        return {"result": result}

    if action == "content":
        html = await page.content()
        text = await page.inner_text("body")
        if len(html) > 50000:
            html = html[:50000] + "..."
        if len(text) > 8000:
            text = text[:8000] + "..."
        return {"html": html, "text": text, "url": page.url}

    # ── New actions ──────────────────────────────────────────────────────
    if action == "scroll":
        if req.selector:
            el = await page.query_selector(req.selector)
            if not el:
                return {"status": "scrolled", "warning": f"selector '{req.selector}' not found"}
            await el.scroll_into_view_if_needed()
            return {"status": "scrolled", "selector": req.selector}
        if req.to == "top":
            await page.evaluate("window.scrollTo(0, 0)")
            return {"status": "scrolled", "to": "top"}
        if req.dy is not None:
            await page.mouse.wheel(req.dx or 0, req.dy)
            return {"status": "scrolled", "dy": req.dy}
        await page.evaluate("window.scrollTo(0, document.body.scrollHeight)")
        return {"status": "scrolled", "to": "bottom"}

    if action == "hover":
        if not req.selector:
            raise HTTPException(400, "selector is required")
        await page.hover(req.selector, timeout=req.timeout * 1000)
        return {"status": "hovered", "selector": req.selector}

    if action == "drag":
        if not req.selector or not req.to_selector:
            raise HTTPException(400, "selector and to_selector are required")
        await page.drag_and_drop(req.selector, req.to_selector, timeout=req.timeout * 1000)
        return {"status": "dragged", "from": req.selector, "to": req.to_selector}

    if action == "back":
        await page.go_back(wait_until="domcontentloaded", timeout=req.timeout * 1000)
        return {"status": "navigated_back", "url": page.url}

    if action == "press":
        if not req.key:
            raise HTTPException(400, "key is required")
        if req.selector:
            await page.press(req.selector, req.key, timeout=req.timeout * 1000)
        else:
            await page.keyboard.press(req.key)
        return {"status": "pressed", "key": req.key}

    if action == "set_dialog":
        st = session_dialog.setdefault(sid, {"accept": True, "prompt_text": None, "last": None})
        if req.accept is not None:
            st["accept"] = req.accept
        st["prompt_text"] = req.prompt_text
        return {"status": "dialog_configured", "accept": st["accept"], "last_dialog": st.get("last")}

    raise HTTPException(400, f"Unknown action: {action}")
