"""Browser CDP — MCP server for headless browser automation via Playwright.

Tools: navigate, click, type, extract_text, screenshot, evaluate.
Follows Immutable Core: runs as external MCP skill in Docker.
"""

import asyncio
import base64
import json
from contextlib import asynccontextmanager

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse
from playwright.async_api import async_playwright, Browser, Page

browser: Browser | None = None
pw_instance = None
# Simple page pool: one page per "session_id" (defaults to "default")
pages: dict[str, Page] = {}


@asynccontextmanager
async def lifespan(app: FastAPI):
    global browser, pw_instance
    pw_instance = await async_playwright().start()
    browser = await pw_instance.chromium.launch(
        headless=True,
        args=["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage"],
    )
    yield
    for p in pages.values():
        try:
            await p.close()
        except Exception:
            pass
    pages.clear()
    await browser.close()
    await pw_instance.stop()


app = FastAPI(title="Browser CDP", lifespan=lifespan)


async def get_page(session_id: str = "default") -> Page:
    """Get or create a browser page for the given session."""
    if session_id in pages and not pages[session_id].is_closed():
        return pages[session_id]
    page = await browser.new_page(
        user_agent="Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
        "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    )
    pages[session_id] = page
    return page


STRIP_SELECTORS = [
    "script", "style", "noscript", "iframe", "svg",
    "nav", "header", "footer", "[role=navigation]",
    "[role=banner]", "[class*=cookie]", "[class*=popup]",
    "[class*=modal]", "[class*=sidebar]",
]

CONTENT_SELECTORS = ["article", "main", "[role=main]", ".content", "#content", "body"]

MCP_TOOLS = [
    {
        "name": "browser_navigate",
        "description": "Navigate to a URL in the headless browser. Returns page title and URL.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL to navigate to"},
                "wait_for": {"type": "string", "description": "CSS selector to wait for after load (optional)"},
                "session_id": {"type": "string", "default": "default", "description": "Browser session ID"},
            },
            "required": ["url"],
        },
    },
    {
        "name": "browser_click",
        "description": "Click on an element matching a CSS selector.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "selector": {"type": "string", "description": "CSS selector of element to click"},
                "session_id": {"type": "string", "default": "default"},
            },
            "required": ["selector"],
        },
    },
    {
        "name": "browser_type",
        "description": "Type text into an input element matching a CSS selector.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "selector": {"type": "string", "description": "CSS selector of input element"},
                "text": {"type": "string", "description": "Text to type"},
                "clear": {"type": "boolean", "default": True, "description": "Clear existing text first"},
                "session_id": {"type": "string", "default": "default"},
            },
            "required": ["selector", "text"],
        },
    },
    {
        "name": "browser_extract_text",
        "description": "Extract readable text content from the current page. Strips navigation, ads, and noise.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "selector": {"type": "string", "description": "CSS selector to extract from (optional, auto-detects main content)"},
                "max_length": {"type": "integer", "default": 8000, "description": "Max text length in chars"},
                "session_id": {"type": "string", "default": "default"},
            },
        },
    },
    {
        "name": "browser_screenshot",
        "description": "Take a screenshot of the current page. Returns base64-encoded PNG.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "full_page": {"type": "boolean", "default": False, "description": "Capture full scrollable page"},
                "selector": {"type": "string", "description": "CSS selector to screenshot (optional, screenshots viewport by default)"},
                "session_id": {"type": "string", "default": "default"},
            },
        },
    },
    {
        "name": "browser_evaluate",
        "description": "Execute JavaScript in the browser page and return the result.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "expression": {"type": "string", "description": "JavaScript expression to evaluate"},
                "session_id": {"type": "string", "default": "default"},
            },
            "required": ["expression"],
        },
    },
    {
        "name": "browser_close",
        "description": "Close a browser session.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "session_id": {"type": "string", "default": "default"},
            },
        },
    },
]


async def handle_navigate(args: dict) -> str:
    page = await get_page(args.get("session_id", "default"))
    url = args["url"]
    await page.goto(url, wait_until="domcontentloaded", timeout=30000)
    if wait_for := args.get("wait_for"):
        try:
            await page.wait_for_selector(wait_for, timeout=10000)
        except Exception:
            pass
    else:
        await page.wait_for_timeout(2000)
    title = await page.title() or ""
    return f"Navigated to: {page.url}\nTitle: {title}"


async def handle_click(args: dict) -> str:
    page = await get_page(args.get("session_id", "default"))
    await page.click(args["selector"], timeout=10000)
    await page.wait_for_timeout(1000)
    return f"Clicked: {args['selector']}"


async def handle_type(args: dict) -> str:
    page = await get_page(args.get("session_id", "default"))
    selector = args["selector"]
    if args.get("clear", True):
        await page.fill(selector, "")
    await page.type(selector, args["text"], delay=50)
    return f"Typed {len(args['text'])} chars into: {selector}"


async def handle_extract_text(args: dict) -> str:
    page = await get_page(args.get("session_id", "default"))
    max_length = args.get("max_length", 8000)

    if selector := args.get("selector"):
        text = await page.evaluate(
            f"""() => {{
                const el = document.querySelector('{selector}');
                return el ? el.innerText : '';
            }}"""
        )
    else:
        # Strip noise
        for sel in STRIP_SELECTORS:
            await page.evaluate(
                f"document.querySelectorAll('{sel}').forEach(el => el.remove())"
            )
        # Find main content
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

    if len(text) > max_length:
        text = text[:max_length] + "..."

    title = await page.title() or ""
    return f"Title: {title}\nURL: {page.url}\n\n{text}"


async def handle_screenshot(args: dict) -> list:
    page = await get_page(args.get("session_id", "default"))
    if selector := args.get("selector"):
        element = page.locator(selector)
        screenshot_bytes = await element.screenshot()
    else:
        screenshot_bytes = await page.screenshot(full_page=args.get("full_page", False))
    b64 = base64.b64encode(screenshot_bytes).decode()
    return [
        {"type": "text", "text": f"Screenshot taken ({len(screenshot_bytes)} bytes)"},
        {"type": "image", "data": b64, "mimeType": "image/png"},
    ]


async def handle_evaluate(args: dict) -> str:
    page = await get_page(args.get("session_id", "default"))
    result = await page.evaluate(args["expression"])
    if isinstance(result, (dict, list)):
        return json.dumps(result, ensure_ascii=False, indent=2)
    return str(result) if result is not None else "(undefined)"


async def handle_close(args: dict) -> str:
    session_id = args.get("session_id", "default")
    if session_id in pages:
        try:
            await pages[session_id].close()
        except Exception:
            pass
        del pages[session_id]
    return f"Session '{session_id}' closed"


TOOL_HANDLERS = {
    "browser_navigate": handle_navigate,
    "browser_click": handle_click,
    "browser_type": handle_type,
    "browser_extract_text": handle_extract_text,
    "browser_screenshot": handle_screenshot,
    "browser_evaluate": handle_evaluate,
    "browser_close": handle_close,
}


@app.get("/health")
async def health():
    return {"status": "ok", "sessions": len(pages)}


@app.post("/mcp")
async def mcp_endpoint(request: Request):
    body = await request.json()
    method = body.get("method", "")
    req_id = body.get("id", 1)
    params = body.get("params", {})

    if method == "tools/list":
        return JSONResponse({
            "jsonrpc": "2.0",
            "result": {"tools": MCP_TOOLS},
            "id": req_id,
        })

    if method == "tools/call":
        tool_name = params.get("name", "")
        args = params.get("arguments", {})

        handler = TOOL_HANDLERS.get(tool_name)
        if not handler:
            return JSONResponse({
                "jsonrpc": "2.0",
                "error": {"code": -32601, "message": f"Unknown tool: {tool_name}"},
                "id": req_id,
            })

        try:
            result = await asyncio.wait_for(handler(args), timeout=60.0)
            # handle_screenshot returns a list of content items
            if isinstance(result, list):
                content = result
            else:
                content = [{"type": "text", "text": result}]
            return JSONResponse({
                "jsonrpc": "2.0",
                "result": {"content": content},
                "id": req_id,
            })
        except asyncio.TimeoutError:
            return JSONResponse({
                "jsonrpc": "2.0",
                "error": {"code": -32000, "message": "Tool execution timed out (60s)"},
                "id": req_id,
            })
        except Exception as e:
            return JSONResponse({
                "jsonrpc": "2.0",
                "error": {"code": -32000, "message": str(e)},
                "id": req_id,
            })

    return JSONResponse({
        "jsonrpc": "2.0",
        "error": {"code": -32601, "message": f"Unknown method: {method}"},
        "id": req_id,
    })
