# toolgate/its/router.py
"""Agent-facing ИТС endpoints. Serialized single session; hard timeout."""
import asyncio
import logging

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from .driver import BrowserDriver
from .flows import ItsFlows, ItsBusy, ItsLoginFailed
from .cache import TTLCache
from .creds import get_credentials
from .site import SITE_ITS

log = logging.getLogger("toolgate.its")
router = APIRouter(tags=["its"])

_lock = asyncio.Lock()
_cache = TTLCache()
_OP_TIMEOUT_S = 90.0


class SearchReq(BaseModel):
    query: str
    db: str | None = None


class ReadReq(BaseModel):
    ref: str


async def build_flows(http) -> ItsFlows:
    return ItsFlows(BrowserDriver(http), SITE_ITS)


async def _run(http, coro_factory):
    creds = await get_credentials(http)
    if not creds:
        return JSONResponse(status_code=502,
                            content={"error": "its_no_credentials",
                                     "message": "ITS_CREDENTIALS не заданы в vault"})
    async with _lock:
        try:
            flows = await build_flows(http)
            await asyncio.wait_for(flows.ensure_logged_in(creds), timeout=_OP_TIMEOUT_S)
            return await asyncio.wait_for(coro_factory(flows), timeout=_OP_TIMEOUT_S)
        except ItsBusy as e:
            return JSONResponse(status_code=409, content={"error": "its_busy", "message": str(e)})
        except ItsLoginFailed as e:
            return JSONResponse(status_code=502, content={"error": "its_login_failed", "message": str(e)})
        except asyncio.TimeoutError:
            return JSONResponse(status_code=504, content={"error": "its_timeout",
                                "message": f"операция превысила {_OP_TIMEOUT_S:.0f}s"})
        except Exception as e:
            log.warning("its error: %s", e)
            return JSONResponse(status_code=502, content={"error": "its_error", "message": str(e)})


@router.post("/its/search")
async def its_search(body: SearchReq, request: Request):
    http = request.app.state.http_client
    ck = f"s:{body.db or ''}:{body.query.strip().lower()}"
    cached = _cache.get(ck)
    if cached is not None:
        return {"results": cached, "cached": True}

    async def do(flows):
        rows = await flows.search(body.query, body.db)
        _cache.set(ck, rows, SITE_ITS["search_cache_ttl_s"])
        return {"results": rows}
    return await _run(http, do)


@router.post("/its/read")
async def its_read(body: ReadReq, request: Request):
    http = request.app.state.http_client
    ck = f"r:{body.ref.strip()}"
    cached = _cache.get(ck)
    if cached is not None:
        return {**cached, "cached": True}

    async def do(flows):
        out = await flows.read(body.ref)
        _cache.set(ck, out, SITE_ITS["read_cache_ttl_s"])
        return out
    return await _run(http, do)
