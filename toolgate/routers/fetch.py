"""URL content fetching & unified web endpoint."""

import logging

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel
from typing import Optional

from helpers import download_limited, clean_html, validate_url_ssrf

log = logging.getLogger("toolgate.fetch")

router = APIRouter(tags=["fetch"])

BROWSER_URL = "http://browser-renderer:9020"


class WebRequest(BaseModel):
    url: str
    mode: str = "read"  # read | render | screenshot
    timeout: int = 15
    full_page: bool = False


async def _read(http, url: str, timeout: int) -> dict:
    """Lightweight fetch + readability extraction."""
    raw_bytes, raw_ct = await download_limited(
        http, url, max_bytes=2 * 1024 * 1024,
        headers={"User-Agent": "OPEX/0.1 (link-preview)"},
        timeout=timeout,
    )
    content_type = raw_ct
    text = raw_bytes.decode("utf-8", errors="replace")

    if "html" in content_type:
        title = ""
        try:
            from readability import Document
            doc = Document(text)
            title = doc.title() or ""
            text = clean_html(doc.summary())
        except Exception:
            log.debug("readability failed, falling back to clean_html")
            text = clean_html(text)

        if len(text) > 8000:
            text = text[:8000] + "...\n[truncated]"
        return {"title": title, "content": text, "url": url}

    if "json" in content_type:
        if len(text) > 8000:
            text = text[:8000] + "...\n[truncated]"
        return {"content": text, "url": url}

    if len(text) > 4000:
        text = text[:4000] + "...\n[truncated]"
    return {"content": text, "url": url}


@router.post("/web")
async def web(body: WebRequest, request: Request):
    """Unified web tool: read (readability), render (Chromium), screenshot (PNG)."""
    http = request.app.state.http_client

    try:
        validate_url_ssrf(body.url)

        if body.mode == "read":
            return await _read(http, body.url, body.timeout)

        elif body.mode == "render":
            resp = await http.post(
                f"{BROWSER_URL}/extract",
                json={"url": body.url, "timeout": body.timeout},
                timeout=body.timeout + 10,
            )
            resp.raise_for_status()
            return resp.json()

        elif body.mode == "screenshot":
            resp = await http.post(
                f"{BROWSER_URL}/screenshot",
                json={"url": body.url, "timeout": body.timeout, "full_page": body.full_page},
                timeout=body.timeout + 10,
            )
            resp.raise_for_status()
            return Response(content=resp.content, media_type="image/png")

        else:
            return JSONResponse(status_code=400, content={"error": f"Unknown mode: {body.mode}. Use: read, render, screenshot"})

    except Exception as e:
        if hasattr(e, 'status_code'):
            raise
        log.warning("Web tool error (mode=%s): %s", body.mode, e)
        return JSONResponse(status_code=502, content={"error": f"Web error: {e}"})
