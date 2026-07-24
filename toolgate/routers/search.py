"""Web search endpoint — resolves a websearch provider (body.provider override or active)."""
import json
from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
import logging

log = logging.getLogger(__name__)
router = APIRouter()

@router.post("/v1/search")
async def web_search(request: Request):
    # request.body() + json.loads() — see embedding.py for the BaseHTTPMiddleware rationale.
    try:
        raw = await request.body()
        body = json.loads(raw) if raw else {}
    except Exception as e:
        return JSONResponse(status_code=400, content={"error": f"invalid JSON body: {e}"})
    query = body.get("query")
    try:
        max_results = int(body.get("max_results") or 5)
    except (TypeError, ValueError):
        max_results = 5
    provider_name = body.get("provider") or None
    if not query:
        return JSONResponse(status_code=400, content={"error": "query is required"})

    registry = request.app.state.registry
    header_chain = [s.strip() for s in
                    (request.headers.get("x-opex-providers") or "").split(",") if s.strip()]
    if provider_name:
        candidates = [await registry.aget_instance(provider_name)]
    elif header_chain:
        candidates = [await registry.aget_instance(n) for n in header_chain]
    else:
        candidates = [await registry.aget_active("websearch")]
    candidates = [c for c in candidates if c is not None]
    if not candidates:
        return JSONResponse(status_code=503, content={
            "error": "no_websearch_provider",
            "hint": "configure/activate a web-search provider in Core UI",
        })

    http = request.app.state.http_client
    last_exc = None
    for provider in candidates:
        try:
            results = await provider.search(http, query, max_results)
            return {"results": results}
        except Exception as e:  # noqa: BLE001 — try next provider in the chain
            log.warning("search provider %s failed: %s", getattr(provider, "name", "?"), e)
            last_exc = e
    # Log full detail server-side; return a generic message (no internal detail leak).
    log.exception("web search failed (all providers)", exc_info=last_exc)
    return JSONResponse(status_code=502, content={"error": "web search failed"})
