"""Web search endpoint — resolves a websearch provider (body.provider override or active)."""
from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
import logging

log = logging.getLogger(__name__)
router = APIRouter()

@router.post("/v1/search")
async def web_search(request: Request):
    body = await request.json()
    query = body.get("query")
    max_results = int(body.get("max_results") or 5)
    provider_name = body.get("provider") or None
    if not query:
        return JSONResponse(status_code=400, content={"error": "query is required"})

    registry = request.app.state.registry
    provider = (
        await registry.aget_instance(provider_name) if provider_name
        else await registry.aget_active("websearch")
    )
    if provider is None:
        return JSONResponse(status_code=503, content={
            "error": "no_websearch_provider",
            "hint": "configure/activate a web-search provider in Core UI",
        })
    try:
        http = request.app.state.http_client
        results = await provider.search(http, query, max_results)
        return {"results": results}
    except Exception as e:
        log.exception("web search failed")
        return JSONResponse(status_code=502, content={"error": str(e)})
