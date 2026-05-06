#!/usr/bin/env python3
"""Verify Core's incoming traceparent extraction.

Send a chat request to Core with a known `traceparent` header. Wait for
spans to flush, then query Jaeger for that exact trace_id and assert it
contains spans from BOTH `hydeclaw-core` and any downstream service.

Without `extract_trace_context_layer`, Core would generate a fresh
trace_id and our supplied trace_id would have zero spans associated. With
the middleware, Core's `pipeline.execute` (and any `POST /v1/embeddings`
etc.) become children of OUR span and Jaeger groups them under our
trace_id.

Run on Pi:
  HYDECLAW_AUTH_TOKEN=<token> python3 test-pi-trace-correlation.py
"""

import json
import os
import secrets
import sys
import time
import urllib.request

PI = os.environ.get("PI_URL", "http://127.0.0.1:18789")
JAEGER = os.environ.get("JAEGER_URL", "http://127.0.0.1:16686")
TOKEN = os.environ["HYDECLAW_AUTH_TOKEN"]
AGENT = "Arty"


def make_traceparent() -> tuple[str, str]:
    """Return (header_value, trace_id) — W3C version-00 format."""
    trace_id = secrets.token_hex(16)         # 128 bits → 32 lowercase hex
    parent_span = secrets.token_hex(8)        # 64 bits → 16 lowercase hex
    return f"00-{trace_id}-{parent_span}-01", trace_id


def post_chat(prompt: str, traceparent: str):
    body = json.dumps({
        "messages": [{"role": "user", "content": prompt}],
        "agent": AGENT,
        "force_new_session": True,
    }).encode("utf-8")
    req = urllib.request.Request(
        f"{PI}/api/chat",
        method="POST",
        data=body,
        headers={
            "Authorization": f"Bearer {TOKEN}",
            "Content-Type": "application/json",
            "traceparent": traceparent,
        },
    )
    return urllib.request.urlopen(req, timeout=60)


def drain(resp, max_events: int = 5):
    """Read at least N SSE events so the request triggers downstream spans."""
    seen = 0
    buf = b""
    while seen < max_events:
        chunk = resp.read1()
        if not chunk:
            return
        buf += chunk
        while b"\n\n" in buf:
            _, _, buf = buf.partition(b"\n\n")
            seen += 1
            if seen >= max_events:
                return


def fetch_trace(trace_id: str) -> dict | None:
    """Look up a trace by id directly. Returns None if Jaeger doesn't have it."""
    url = f"{JAEGER}/api/traces/{trace_id}"
    try:
        with urllib.request.urlopen(url, timeout=10) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise


def main():
    print("=== Trace propagation test: external traceparent → Core → downstream ===\n")

    traceparent, trace_id = make_traceparent()
    print(f"[1] Sending request with traceparent: {traceparent}")
    print(f"    Expected trace_id in Jaeger: {trace_id}\n")

    resp = post_chat("найди новости", traceparent)
    drain(resp, max_events=5)
    resp.close()

    print("[2] Waiting 8s for OTel batch exporter to flush spans...")
    time.sleep(8)

    print("[3] Querying Jaeger for the supplied trace_id...")
    data = fetch_trace(trace_id)
    if not data or not data.get("data"):
        print(f"❌ Jaeger has no trace with id={trace_id}")
        print("   This means Core did NOT honour the incoming traceparent.")
        return 1

    trace = data["data"][0]
    spans = trace.get("spans", [])
    processes = trace.get("processes", {})
    services = sorted({
        v.get("serviceName", "?") for v in processes.values() if isinstance(v, dict)
    })

    print(f"\n✅ Jaeger trace_id={trace_id[:16]}... found")
    print(f"   spans: {len(spans)}")
    print(f"   services: {services}")

    pipeline_spans = [s for s in spans if s["operationName"].startswith("pipeline.")]
    print(f"   pipeline.* spans: {len(pipeline_spans)}")

    if not pipeline_spans:
        print("\n❌ trace exists but has no pipeline.* spans — middleware extracted")
        print("   traceparent but pipeline didn't emit children under it.")
        return 1

    if "hydeclaw-core" not in services:
        print("\n❌ trace doesn't contain hydeclaw-core spans")
        return 1

    print("\n✅ TRACE CORRELATION PASSED — external traceparent honoured")
    return 0


if __name__ == "__main__":
    sys.exit(main())
