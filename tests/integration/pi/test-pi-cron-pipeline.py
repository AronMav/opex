#!/usr/bin/env python3
"""Verify cron jobs route through pipeline::execute (Phase 7a).

After Phase 6b, every caller of `handle_isolated` is replaced with
`handle_isolated_via_pipeline`, which goes through bootstrap + execute +
finalize like the SSE path. This script:

1. Creates a one-shot cron job via `POST /api/cron`.
2. Triggers it manually via `POST /api/cron/{id}/run`.
3. Polls Jaeger for new `pipeline.execute` spans.
4. Confirms the cron run row in `cron_runs` reached `success`/`error`.
5. Cleans up the test cron job.

Run on Pi:
  OPEX_AUTH_TOKEN=<token> python3 test-pi-cron-pipeline.py
"""
import json
import os
import sys
import time
import urllib.error
import urllib.request

PI = os.environ.get("PI_URL", "http://127.0.0.1:18789")
JAEGER = os.environ.get("JAEGER_URL", "http://127.0.0.1:16686")
TOKEN = os.environ["OPEX_AUTH_TOKEN"]
AGENT = os.environ.get("AGENT", "Arty")


def api(path: str, method: str = "GET", body: dict | None = None):
    h = {"Authorization": f"Bearer {TOKEN}"}
    data = None
    if body is not None:
        h["Content-Type"] = "application/json"
        data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(f"{PI}{path}", method=method, data=data, headers=h)
    return urllib.request.urlopen(req, timeout=30)


def jaeger_search(operation: str, lookback: str = "10m") -> list[dict]:
    """List recent traces for a given operation on opex-core service."""
    url = (
        f"{JAEGER}/api/traces?service=opex-core"
        f"&operation={operation}&limit=20&lookback={lookback}"
    )
    try:
        with urllib.request.urlopen(url, timeout=10) as r:
            d = json.load(r)
            return d.get("data") or []
    except urllib.error.HTTPError:
        return []


def main() -> int:
    failures = 0
    print("=== Phase 7 - pipeline route validation for cron jobs ===\n")

    pre_count = len(jaeger_search("pipeline.execute"))
    print(f"[1] Baseline: {pre_count} pipeline.execute spans in last 10m")

    print(f"\n[2] Creating one-shot cron job for agent={AGENT}...")
    create_body = {
        "agent_id": AGENT,
        "name": f"phase7-validation-{int(time.time())}",
        "cron_expr": "0 0 0 1 1 *",
        "timezone": "UTC",
        "task_message": "say 'pipeline-validation-ok' and stop",
        "enabled": False,
        "silent": True,
    }
    resp = api("/api/cron", method="POST", body=create_body)
    job = json.load(resp)
    job_id = job["id"]
    print(f"  job_id = {job_id}")

    try:
        print("\n[3] Triggering manual run...")
        api(f"/api/cron/{job_id}/run", method="POST")
        print("  trigger accepted; waiting up to 90s for completion...")

        deadline = time.time() + 90
        run_status = None
        while time.time() < deadline:
            try:
                with api(f"/api/cron/{job_id}/runs") as r:
                    runs = json.load(r)
            except urllib.error.HTTPError:
                runs = []
            if runs:
                latest = runs[0]
                status = latest.get("status")
                if status in ("success", "error"):
                    run_status = status
                    print(f"  run finished with status = {status}")
                    if status == "error":
                        print(f"  error preview: {(latest.get('error') or '')[:200]}")
                    break
            time.sleep(3)

        if run_status is None:
            print("WARN run did not finish within 90s")
            failures += 1
        else:
            print(f"OK cron run terminated cleanly ({run_status})")

        print("\n[4] Waiting 8s for OTel batch exporter flush...")
        time.sleep(8)

        post_count = len(jaeger_search("pipeline.execute"))
        delta = post_count - pre_count
        print(f"[5] Post-run pipeline.execute spans in last 10m: {post_count} (+{delta})")
        if delta >= 1:
            print(
                "OK at least one new pipeline.execute span landed in Jaeger - "
                "cron route engages the unified pipeline"
            )
        else:
            print(
                "FAIL no new pipeline.execute span observed - cron path NOT "
                "routed through pipeline::execute"
            )
            failures += 1

        finalize_traces = jaeger_search("pipeline.finalize", lookback="5m")
        if any(
            any(
                tag["key"] == "outcome"
                and tag["value"] in ("done", "failed", "interrupted")
                for s in t.get("spans", [])
                if s["operationName"] == "pipeline.finalize"
                for tag in s.get("tags", [])
            )
            for t in finalize_traces
        ):
            print(
                "OK pipeline.finalize span carries outcome={done|failed|interrupted} "
                "- finalize path engaged"
            )
        else:
            print("FAIL no pipeline.finalize span with outcome field found")
            failures += 1

    finally:
        print(f"\n[6] Cleaning up test cron job {job_id}...")
        try:
            api(f"/api/cron/{job_id}", method="DELETE")
            print("  deleted")
        except urllib.error.HTTPError as e:
            print(f"  cleanup HTTP {e.code} (non-fatal)")

    print("\n" + "=" * 50)
    if failures == 0:
        print("OK PHASE 7 VALIDATION PASSED - cron jobs route through pipeline::execute")
        return 0
    print(f"FAIL {failures} check(s) failed")
    return 1


if __name__ == "__main__":
    sys.exit(main())
