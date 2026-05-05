"""Phase 62 RES-07: Verify toolgate asyncio tuning.

- httpx.Limits is constructed with max_connections=20, max_keepalive_connections=10.
- The app.py source references --limit-concurrency indirectly (it's enforced by
  uvicorn at the entrypoint; we verify app.py passes limits to AsyncClient here).
"""
import re
from pathlib import Path

import httpx


def test_httpx_limits_constants_correct():
    """Construct Limits with the Phase 62 RES-07 values and verify them."""
    limits = httpx.Limits(max_connections=20, max_keepalive_connections=10)
    assert limits.max_connections == 20
    assert limits.max_keepalive_connections == 10


def test_app_py_uses_phase62_res07_limits():
    """Read app.py source and confirm the tuned limits are wired into lifespan."""
    app_py = Path(__file__).parent / "app.py"
    assert app_py.exists(), f"app.py not found at {app_py}"
    src = app_py.read_text(encoding="utf-8")

    # httpx.Limits must be passed to AsyncClient.
    assert "httpx.Limits(" in src, (
        "app.py must pass httpx.Limits to AsyncClient (Phase 62 RES-07)"
    )
    # Verify exact max_connections value.
    assert re.search(r"max_connections\s*=\s*20", src), (
        "max_connections=20 must appear in app.py (Phase 62 RES-07)"
    )
    assert re.search(r"max_keepalive_connections\s*=\s*10", src), (
        "max_keepalive_connections=10 must appear in app.py (Phase 62 RES-07)"
    )
    # Pool timeout 120s must be preserved on the shared AsyncClient — that's
    # how PoolTimeout fires when the per-provider max_connections=20 queue
    # backs up. The current code uses `httpx.Timeout(connect=10.0, read=None,
    # write=10.0, pool=120.0)` (more precise than the original flat
    # `timeout=120.0`); the 120s pool wait is what this test guards.
    assert re.search(r"pool\s*=\s*120\.0", src), (
        "pool=120.0 must be preserved in app.py's httpx.Timeout(...) "
        "so PoolTimeout still fires after the queue-backpressure window"
    )


def test_app_py_does_not_bypass_single_worker_mandate():
    """app.py must not spin up its own multi-worker state (CLAUDE.md mandate)."""
    app_py = Path(__file__).parent / "app.py"
    src = app_py.read_text(encoding="utf-8")
    # Guard against accidental uvicorn.run(..., workers=N) with N > 1.
    forbidden = re.search(r"workers\s*=\s*(?!1\b)\d+", src)
    assert forbidden is None, (
        f"app.py must not specify workers=N for N>1; found: {forbidden.group(0) if forbidden else None}"
    )
