"""Image Generation endpoint."""

import inspect
import logging
import os
import re
import time

from fastapi import APIRouter, Request, Depends
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel
from typing import Optional

import httpx

from dependencies import require_provider
from helpers import log_provider

log = logging.getLogger("toolgate.imagegen")

router = APIRouter(tags=["imagegen"])

# ── Rate limiting ─────────────────────────────────────────────────────────────
IMAGE_BUDGET_PER_HOUR = int(os.environ.get("IMAGE_BUDGET_PER_HOUR", "50"))
_rate_limit_count = 0
_rate_limit_reset_at = time.monotonic() + 3600.0

_SIZE_RE = re.compile(r"^\d+x\d+$")


class ImageGenRequest(BaseModel):
    prompt: str
    size: Optional[str] = "1024x1024"
    model: Optional[str] = None
    quality: Optional[str] = "standard"
    negative_prompt: Optional[str] = None


@router.post("/generate-image")
async def generate_image(
    body: ImageGenRequest,
    request: Request,
    provider=Depends(require_provider("imagegen")),
):
    global _rate_limit_count, _rate_limit_reset_at

    log_provider(log, provider)

    # ── Size validation ───────────────────────────────────────────────────────
    size = body.size
    if not _SIZE_RE.match(size):
        return JSONResponse(
            status_code=400,
            content={"error": f"Invalid size format '{size}': expected NNNxNNN (e.g. 1024x1024)"},
        )

    # ── Per-hour budget check ─────────────────────────────────────────────────
    now = time.monotonic()
    if now >= _rate_limit_reset_at:
        _rate_limit_count = 0
        _rate_limit_reset_at = now + 3600.0

    if _rate_limit_count >= IMAGE_BUDGET_PER_HOUR:
        seconds_left = int(_rate_limit_reset_at - now)
        return JSONResponse(
            status_code=429,
            content={
                "error": (
                    f"Image generation rate limit exceeded: {IMAGE_BUDGET_PER_HOUR} images/hour. "
                    f"Resets in {seconds_left}s."
                )
            },
        )

    _rate_limit_count += 1

    # Pass negative_prompt only to providers that accept it (e.g. ComfyUI) —
    # the other imagegen drivers keep their 5-arg generate() signature.
    extra = {}
    if body.negative_prompt and "negative_prompt" in inspect.signature(provider.generate).parameters:
        extra["negative_prompt"] = body.negative_prompt

    try:
        image_bytes = await provider.generate(
            request.app.state.http_client, body.prompt,
            size, body.model,
            body.quality or "standard",
            **extra,
        )
        return Response(content=image_bytes, media_type="image/png")
    except httpx.HTTPStatusError as e:
        return JSONResponse(status_code=e.response.status_code,
                            content={"error": f"ImageGen error: {e.response.text}"})
    except Exception as e:
        return JSONResponse(status_code=502, content={"error": f"ImageGen error: {e}"})
