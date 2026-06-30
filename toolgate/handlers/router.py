"""FastAPI routes for the file-handler hub.

GET /handlers          — manifests + ETag (304 on If-None-Match), mirrors the
                         ProviderRegistry discovery contract. Each manifest's
                         `provider` is filled from the active provider when the
                         handler declares a `capability` (R5).
GET /handlers/{id}     — single manifest (debug/UI).
POST /handlers/{id}/run — execute. R12: this is MULTIPART form-data; the upload
                         bytes arrive in the `file` field (core downloaded them
                         in Rust and POSTed them — toolgate NEVER fetches a
                         loopback url). SYNC handlers run inline under a
                         per-execution timeout and return a ScenarioOutcome
                         json. ASYNC handlers spawn an out-of-process runner —
                         that branch is added in Phase 5 (R10 extends THIS fn);
                         until then it returns 501 so the sync path is fully
                         testable.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os

from fastapi import APIRouter, File, Form, Request, Response, UploadFile
from fastapi.responses import JSONResponse

from handlers.context import HandlerFile, build_context

log = logging.getLogger("toolgate.handlers")

# R5: hard ceiling on a single sync handler execution.
HANDLER_SYNC_TIMEOUT_SECS = 120.0

router = APIRouter(tags=["handlers"])


def _core_url() -> str:
    return os.environ.get("CORE_API_URL", "http://127.0.0.1:18789")


async def _manifests_with_provider(request: Request) -> list[dict]:
    """Manifests with `provider` filled from the active provider per R5."""
    registry = request.app.state.handlers
    provider_registry = request.app.state.registry
    out: list[dict] = []
    for m in registry.manifests():
        cap = m.get("capability")
        if cap:
            try:
                active = await provider_registry.aget_active(cap)
            except Exception:  # provider lookup is best-effort for discovery
                active = None
            m = {**m, "provider": getattr(active, "name", None) if active else None}
        out.append(m)
    return out


@router.get("/handlers")
async def list_handlers(request: Request, response: Response):
    registry = request.app.state.handlers
    etag = registry.etag()
    inm = request.headers.get("if-none-match")
    if inm and inm == etag:
        return Response(status_code=304, headers={"ETag": etag})
    response.headers["ETag"] = etag
    return {"handlers": await _manifests_with_provider(request), "etag": etag}


@router.get("/handlers/{handler_id}")
async def get_handler(handler_id: str, request: Request):
    registry = request.app.state.handlers
    if registry.get(handler_id) is None:
        return JSONResponse(status_code=404, content={"error": "handler_not_found"})
    for m in await _manifests_with_provider(request):
        if m["id"] == handler_id:
            return m
    return JSONResponse(status_code=404, content={"error": "handler_not_found"})


@router.post("/handlers/{handler_id}/run")
async def run_handler(
    handler_id: str,
    request: Request,
    file: UploadFile | None = File(default=None),
    mime: str = Form(...),
    filename: str = Form(...),
    params: str = Form(default="{}"),
    language: str = Form(default="ru"),
    job_id: str | None = Form(default=None),
    source_url: str | None = Form(default=None),
):
    registry = request.app.state.handlers
    lh = registry.get(handler_id)
    if lh is None:
        return JSONResponse(status_code=404, content={"error": "handler_not_found"})

    descriptor = lh.descriptor
    if descriptor.execution == "async":
        # Out-of-process runner is delivered in Phase 5 (R10 extends THIS fn).
        return JSONResponse(status_code=501,
                            content={"error": "async_runner_not_available"})

    # R12: bytes arrive in the multipart `file` field — never fetched here.
    data = await file.read() if file is not None else b""

    try:
        parsed_params = json.loads(params) if params else {}
    except json.JSONDecodeError:
        parsed_params = {}
    if not isinstance(parsed_params, dict):
        parsed_params = {}
    parsed_params.setdefault("language", language)

    f = HandlerFile(bytes=data, mime=mime, filename=filename, size=len(data),
                    source_url=source_url)
    http = request.app.state.http_client
    ctx = build_context(request.app.state.registry, http, core_url=_core_url())

    # R5: per-execution timeout on the handler body.
    try:
        result = await asyncio.wait_for(lh.run(ctx, f, parsed_params),
                                        timeout=HANDLER_SYNC_TIMEOUT_SECS)
    except asyncio.TimeoutError:
        return JSONResponse(status_code=200, content={
            "status": "timeout", "summary_text": "",
            "artifact_urls": [], "reason": "per-execution timeout",
        })
    except Exception as e:
        log.exception("handler %s failed", handler_id)
        return JSONResponse(status_code=200, content=ctx.result.failed(str(e)).to_dict())
    return JSONResponse(status_code=200, content=result.to_dict())
