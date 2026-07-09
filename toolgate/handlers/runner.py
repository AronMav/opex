"""Out-of-process handler-runner (R12). Launched per async job by router.py via
`python -m handlers.runner '<json spec>'`.

Reads the JSON job spec from argv[1] (or stdin), rebuilds the registry + ctx,
loads the file bytes FROM THE LOCAL TEMP PATH (never a loopback fetch) or uses
source_url for url-based handlers, runs the handler, and posts progress + the
final ScenarioOutcome to the core callbacks. Deletes the temp file in finally.
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
import sys
from pathlib import Path

import httpx

# Ensure the toolgate package root is importable when invoked as a subprocess
# via `python -m handlers.runner` from any working directory.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from handlers.loader import HandlerRegistry  # noqa: E402
from handlers.context import build_context, HandlerFile  # noqa: E402
from registry import ProviderRegistry  # noqa: E402

log = logging.getLogger("toolgate.runner")

_BUILTIN_DIR = str(Path(__file__).resolve().parent / "builtin")


def _load_registry(http: httpx.AsyncClient) -> HandlerRegistry:
    """Rebuild the handler registry in the subprocess.

    Only builtins are loaded here; workspace handlers are added in a future
    phase when the workspace path is passed in the job spec.
    """
    reg = HandlerRegistry()
    reg.load_all(builtin_dir=_BUILTIN_DIR, workspace_dir=None)
    return reg


def _outcome_dict(outcome) -> dict:
    """Normalise any outcome value to the 4-key wire dict."""
    if hasattr(outcome, "to_dict"):
        return outcome.to_dict()
    if isinstance(outcome, dict):
        return outcome
    return {
        "status": getattr(outcome, "status", "failed"),
        "summary_text": getattr(outcome, "summary_text", ""),
        "artifact_urls": getattr(outcome, "artifact_urls", []),
        "reason": getattr(outcome, "reason", None),
    }


# F016: finite wall-clock backstop for the whole handler body — a stalled
# provider or a CPU-bound handler must not wedge the runner subprocess forever.
# Sized for long-video digests (hours) but finite; core's worker stale-sweep
# (F014, deadline 4h) reaps the row only well past this.
JOB_WALL_CLOCK_SECS = 3 * 3600  # 3h

# F015: the final /complete POST is the ONLY signal that moves the job out of
# 'processing'. A single fire-and-forget POST that ignores a transient non-2xx
# silently discards the result and wedges the row — retry with bounded backoff.
_COMPLETE_MAX_RETRIES = 4


async def _post_complete(
    http: httpx.AsyncClient,
    core_url: str,
    job_id: str,
    headers: dict,
    payload: dict,
) -> None:
    """POST the final outcome to core /complete with status check + bounded
    retry (F015). Core marks the job done/failed ONLY on a 2xx here, so a
    dropped/transient-error callback must be retried, not ignored."""
    last_err: str | None = None
    for attempt in range(_COMPLETE_MAX_RETRIES):
        if attempt:
            await asyncio.sleep(min(2 ** attempt, 15))
        try:
            resp = await http.post(
                f"{core_url}/api/files/jobs/{job_id}/complete",
                headers=headers,
                json=payload,
                timeout=30.0,  # F016: explicit finite timeout, not the client's read
            )
            if 200 <= resp.status_code < 300:
                return
            # An auth/permission rejection won't recover on retry (expired token).
            if resp.status_code in (401, 403):
                log.error("job %s /complete rejected %s — giving up", job_id, resp.status_code)
                return
            last_err = f"HTTP {resp.status_code}"
            log.warning("job %s /complete → %s (attempt %d)", job_id, resp.status_code, attempt + 1)
        except Exception as exc:  # noqa: BLE001 — retry any transport error
            last_err = str(exc)
            log.warning("job %s /complete post failed (attempt %d): %s", job_id, attempt + 1, exc)
    log.error(
        "job %s /complete FAILED after %d attempts (%s) — result lost until core sweep reaps it",
        job_id, _COMPLETE_MAX_RETRIES, last_err,
    )


async def run_job(spec: dict) -> None:
    """Execute one async handler job end-to-end.

    Posts progress updates and the final ScenarioOutcome to the core callback
    endpoints. Deletes the temp file in a finally block regardless of outcome.
    """
    job_id = spec["job_id"]
    core_url = spec["core_url"].rstrip("/")
    auth = spec.get("auth_token", "")
    headers: dict[str, str] = {"Authorization": f"Bearer {auth}"} if auth else {}
    # FIX 5: forward the per-job HMAC token so core can verify the caller is
    # the legitimate runner for this job (Task 5 mints and injects it into the
    # spec; if absent, the header is omitted — core returns 401 in that case).
    callback_token = spec.get("callback_token")
    if callback_token:
        headers["X-Job-Token"] = callback_token
    temp_path = spec.get("temp_path")

    # ProviderRegistry._refresh() reads OPEX_AUTH_TOKEN + CORE_API_URL from the
    # environment. The subprocess normally inherits toolgate's env, but seed the
    # token from the spec too so capability resolution works even if it doesn't.
    if auth:
        os.environ["OPEX_AUTH_TOKEN"] = auth

    try:
        # F016: finite read timeout (was read=None). A provider/core endpoint
        # that accepts the connection but never sends a body must fail the call,
        # not block the runner forever. 300s per-read tolerates slow STT/LLM
        # streaming while still bounding a black-holed socket.
        async with httpx.AsyncClient(
            timeout=httpx.Timeout(connect=10.0, read=300.0, write=10.0, pool=120.0)
        ) as http:
            registry = _load_registry(http)
            loaded = registry.get(spec["handler_id"])
            if loaded is None:
                await _post_complete(
                    http, core_url, job_id, headers,
                    {
                        "status": "failed",
                        "summary_text": "",
                        "artifact_urls": [],
                        "reason": f"unknown handler {spec['handler_id']}",
                    },
                )
                return

            # Build the live progress callback — posts to core over loopback
            # (trusted internal endpoint, NOT subject to SSRF guard).
            async def progress_cb(phase: str, pct: int) -> None:
                try:
                    await http.post(
                        f"{core_url}/api/files/jobs/{job_id}/progress",
                        headers=headers,
                        json={"phase": phase, "pct": pct},
                    )
                except Exception as exc:  # progress is best-effort
                    log.warning("progress post failed: %s", exc)

            # The HandlerRegistry above only resolves WHICH handler to run. The
            # ctx capabilities (ctx.stt / ctx.vision / ctx.tts / …) resolve
            # ACTIVE PROVIDERS via a ProviderRegistry — build_context wires each
            # _CapabilityWrapper to call registry.aget_active(cap). Passing the
            # HandlerRegistry here raised `'HandlerRegistry' object has no
            # attribute 'aget_active'` and broke every async handler (e.g.
            # summarize_video's ctx.stt.transcribe). Build a real ProviderRegistry.
            provider_registry = ProviderRegistry()
            await provider_registry.aload()

            ctx = build_context(
                provider_registry, http,
                job_id=job_id,
                core_url=core_url,
                auth_token=auth,
                config=spec.get("config") or {},
            )
            # Rebind ctx.progress so it posts over the live http client for
            # this job. The default HandlerContext.progress already does this
            # internally; we override it here for the subprocess so the closure
            # captures the correct http client and headers.
            ctx.progress = progress_cb  # type: ignore[method-assign]

            # R12: read bytes from the local temp path written by the router.
            # Never fetch a loopback URL — toolgate's SSRF guard blocks it.
            data = b""
            if temp_path and os.path.exists(temp_path):
                data = Path(temp_path).read_bytes()

            handler_file = HandlerFile(
                bytes=data,
                mime=spec.get("mime") or "application/octet-stream",
                filename=spec.get("filename") or "file",
                size=len(data),
                source_url=spec.get("source_url"),
            )

            try:
                # F016: bound the handler body with an overall wall-clock so a
                # stalled provider or a runaway handler can't wedge the runner
                # (and pin the job in 'processing') forever.
                outcome = await asyncio.wait_for(
                    loaded.run(ctx, handler_file, spec.get("params") or {}),
                    timeout=JOB_WALL_CLOCK_SECS,
                )
                payload = _outcome_dict(outcome)
            except asyncio.TimeoutError:
                log.error(
                    "handler %s exceeded %ds wall-clock limit",
                    spec["handler_id"], JOB_WALL_CLOCK_SECS,
                )
                payload = {
                    "status": "failed",
                    "summary_text": "",
                    "artifact_urls": [],
                    "reason": f"handler exceeded {JOB_WALL_CLOCK_SECS}s wall-clock limit",
                }
            except Exception as exc:
                log.exception("handler %s run failed", spec["handler_id"])
                payload = {
                    "status": "failed",
                    "summary_text": "",
                    "artifact_urls": [],
                    "reason": str(exc),
                }

            await _post_complete(http, core_url, job_id, headers, payload)
    finally:
        if temp_path and os.path.exists(temp_path):
            try:
                os.unlink(temp_path)
            except OSError as exc:
                log.warning("temp file cleanup failed: %s", exc)


def main() -> None:
    """CLI entry point: `python -m handlers.runner '<json spec>'`."""
    raw = sys.argv[1] if len(sys.argv) > 1 else sys.stdin.read()
    asyncio.run(run_job(json.loads(raw)))


if __name__ == "__main__":
    main()
