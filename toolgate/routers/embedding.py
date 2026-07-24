"""Embedding endpoint - proxies to active embedding provider."""
import json
from fastapi import APIRouter, Depends, Request
from fastapi.responses import JSONResponse
import logging

from dependencies import require_provider

log = logging.getLogger(__name__)
router = APIRouter()


@router.post("/v1/embeddings")
async def embeddings(
    request: Request,
    provider=Depends(require_provider("embedding")),
):
    # Read raw body + json.loads instead of request.json() to avoid
    # Starlette BaseHTTPMiddleware body corruption (two stacked
    # BaseHTTPMiddleware instances corrupt the receive channel in
    # Starlette 0.47.x, turning valid JSON into garbage at request.json()).
    try:
        raw = await request.body()
        body = json.loads(raw) if raw else {}
    except Exception as e:
        log.warning("embedding: failed to parse request body (%s), raw[:100]=%s", e, raw[:100] if raw else b"")
        return JSONResponse(
            status_code=400,
            content={"error": f"invalid JSON body: {e}"},
        )

    texts = body.get("input", [])
    model = body.get("model")

    if isinstance(texts, str):
        texts = [texts]

    if not texts:
        return JSONResponse(
            status_code=400,
            content={"error": "input is required"},
        )

    try:
        http = request.app.state.http_client
        vectors = await provider.embed(http, texts, model)
        data = [
            {"object": "embedding", "index": i, "embedding": vec}
            for i, vec in enumerate(vectors)
        ]
        actual_model = model or getattr(provider, "model", "") or ""
        return {"object": "list", "data": data, "model": actual_model}
    except Exception as e:
        log.exception("embedding failed")
        return JSONResponse(
            status_code=502,
            content={"error": str(e)},
        )
