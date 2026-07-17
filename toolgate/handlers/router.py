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
import sys
import tempfile

from fastapi import APIRouter, Body, File, Form, Request, Response, UploadFile
from fastapi.responses import JSONResponse

from handlers.context import HandlerFile, build_context
from handlers.validate import validate_source

# Absolute path to runner.py — used to spawn the out-of-process runner via
# `sys.executable -m handlers.runner` (portable across dev/prod venvs).
_RUNNER_MODULE = "handlers.runner"

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


@router.post("/handlers/validate")
async def validate_handler(payload: dict = Body(...)):
    source = payload.get("source")
    if not isinstance(source, str):
        return JSONResponse(status_code=400, content={"error": "missing 'source'"})
    expected_id = payload.get("id")
    if expected_id is not None and not isinstance(expected_id, str):
        expected_id = None
    return validate_source(source, expected_id)


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
    callback_token: str | None = Form(default=None),
    config: str = Form(default="{}"),
):
    registry = request.app.state.handlers
    lh = registry.get(handler_id)
    if lh is None:
        return JSONResponse(status_code=404, content={"error": "handler_not_found"})

    # Operator-set per-agent settings (valves) forwarded by core as a JSON blob.
    # `isinstance(config, str)` guards the unit-test path that calls run_handler
    # directly (there `config` is the unresolved FastAPI Form sentinel, not a str).
    try:
        parsed_config = json.loads(config) if isinstance(config, str) and config else {}
    except json.JSONDecodeError:
        parsed_config = {}
    if not isinstance(parsed_config, dict):
        parsed_config = {}

    descriptor = lh.descriptor
    if descriptor.execution == "async":
        # R12: persist upload bytes to a tempfile so the out-of-process runner
        # can read the PATH (never a loopback signed URL). For url-based async
        # handlers (video from source_url) no bytes are uploaded so no tempfile.
        upload_bytes = await file.read() if file is not None else b""
        temp_path: str | None = None
        if upload_bytes:
            tf = tempfile.NamedTemporaryFile(prefix="opex-handler-", delete=False)
            tf.write(upload_bytes)
            tf.close()
            temp_path = tf.name

        try:
            parsed_params = json.loads(params) if params else {}
        except json.JSONDecodeError:
            parsed_params = {}
        if not isinstance(parsed_params, dict):
            parsed_params = {}

        spec = {
            "handler_id": handler_id,
            "temp_path": temp_path,
            "source_url": source_url,
            "mime": mime,
            "filename": filename,
            "params": parsed_params,
            "config": parsed_config,
            "language": language,
            "job_id": job_id,
            "core_url": os.environ.get("CORE_API_URL", "http://127.0.0.1:18789"),
            "auth_token": os.environ.get("OPEX_AUTH_TOKEN", ""),
            "callback_token": callback_token,
            # F050: carry the workspace dir so the out-of-process runner loads
            # the SAME workspace handlers (and builtin overrides) as this main
            # process — otherwise async workspace handlers fail as "unknown
            # handler" and async builtin overrides silently run the pristine
            # builtin.
            "workspace_dir": getattr(
                getattr(request.app.state.registry, "config", None), "workspace_dir", None
            ),
        }
        # F026: pipe the spec over stdin, NOT argv. spec carries the master
        # OPEX_AUTH_TOKEN and the per-job HMAC callback_token; on argv they are
        # world-readable via /proc/<pid>/cmdline and `ps auxww` for the entire
        # (possibly hours-long) runner lifetime. runner.main() already reads the
        # spec from stdin when no argv is given.
        proc = await asyncio.create_subprocess_exec(
            sys.executable, "-m", _RUNNER_MODULE,
            stdin=asyncio.subprocess.PIPE,
        )
        if proc.stdin is not None:
            proc.stdin.write(json.dumps(spec).encode())
            await proc.stdin.drain()
            proc.stdin.close()
        return JSONResponse(status_code=202, content={"accepted": True, "job_id": job_id})

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
    ctx = build_context(request.app.state.registry, http, core_url=_core_url(),
                        config=parsed_config)

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
