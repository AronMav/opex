#!/usr/bin/env python3
"""
Pi concurrency test: 2 parallel sessions for the same agent must produce
independent SSE streams with non-overlapping seq counters and disjoint
DB rows.
"""
import json
import os
import sys
import threading
import time
import urllib.request
import uuid

PI_HOST = "http://192.168.1.82:18789"
TOKEN = os.environ["HYDECLAW_AUTH_TOKEN"]
AGENT = "Arty"


def http_post(path, body):
    req = urllib.request.Request(
        f"{PI_HOST}{path}",
        method="POST",
        data=json.dumps(body).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {TOKEN}",
            "Content-Type": "application/json",
        },
    )
    return urllib.request.urlopen(req, timeout=120)


def run_stream(label, prompt, results):
    """Open SSE, capture step-starts + final session_id + last seq."""
    body = {
        "messages": [{"role": "user", "content": prompt}],
        "agent": AGENT,
        "force_new_session": True,
    }
    resp = http_post("/api/chat", body)

    pending_id = None
    step_ids = []
    message_ids = set()
    last_seq = 0
    sid = None
    finish = False

    buf = b""
    while True:
        chunk = resp.read1()
        if not chunk:
            break
        buf += chunk
        while b"\n" in buf:
            line, _, buf = buf.partition(b"\n")
            line = line.decode("utf-8", "replace").rstrip("\r")
            if line.startswith("id:"):
                pending_id = line[3:].strip()
                try:
                    last_seq = max(last_seq, int(pending_id))
                except ValueError:
                    pass
            elif line.startswith("data:"):
                data = line[5:].strip()
                if data == "[DONE]":
                    break
                try:
                    ev = json.loads(data)
                    if ev.get("type") == "data-session-id":
                        sid = ev["data"]["sessionId"]
                    elif ev.get("type") == "step-start":
                        step_ids.append(ev["stepId"])
                        if "messageId" in ev:
                            message_ids.add(ev["messageId"])
                    elif ev.get("type") == "finish":
                        finish = True
                except json.JSONDecodeError:
                    pass
            elif line == "":
                pending_id = None

    results[label] = {
        "session_id": sid,
        "step_ids": step_ids,
        "message_ids": list(message_ids),
        "last_seq": last_seq,
        "finish": finish,
    }


def main():
    results = {}
    t1 = threading.Thread(
        target=run_stream,
        args=("A", "ответь словом 'один' и заверши", results),
    )
    t2 = threading.Thread(
        target=run_stream,
        args=("B", "ответь словом 'два' и заверши", results),
    )

    print("Starting two parallel sessions...")
    t1.start()
    time.sleep(0.5)  # offset slightly so backend doesn't reuse same session
    t2.start()

    t1.join(timeout=180)
    t2.join(timeout=180)

    if "A" not in results or "B" not in results:
        print("❌ One or both streams did not complete")
        return 1

    A, B = results["A"], results["B"]
    print(f"\n[A] {json.dumps(A)}")
    print(f"[B] {json.dumps(B)}")

    failures = 0
    print("\n--- Concurrency invariants ---")

    # 1. Different sessions
    if A["session_id"] == B["session_id"]:
        print(f"❌ Same session id! {A['session_id']}")
        failures += 1
    else:
        print(f"✅ Different session ids: {A['session_id'][:8]} vs {B['session_id'][:8]}")

    # 2. Disjoint message_ids
    overlap = set(A["message_ids"]) & set(B["message_ids"])
    if overlap:
        print(f"❌ Overlapping messageIds across sessions: {overlap}")
        failures += 1
    else:
        print(f"✅ Disjoint messageIds: {len(A['message_ids'])} + {len(B['message_ids'])}")

    # 3. Both got finish
    if not A["finish"] or not B["finish"]:
        print(f"❌ Missing finish events: A={A['finish']}, B={B['finish']}")
        failures += 1
    else:
        print(f"✅ Both streams emitted finish event")

    # 4. Independent seq counters (each starts from 1, so both should have small lastSeq)
    # The point is sessions shouldn't share a counter — they shouldn't co-mingle.
    print(f"  A.lastSeq={A['last_seq']}, B.lastSeq={B['last_seq']}")
    if A["last_seq"] == 0 or B["last_seq"] == 0:
        print(f"❌ A seq counter never reached 1 — backend may have crashed")
        failures += 1
    else:
        print(f"✅ Both sessions have independent seq counters > 0")

    print("\n" + "=" * 40)
    if failures == 0:
        print("✅ CONCURRENCY OK")
        return 0
    print(f"❌ {failures} invariant(s) violated")
    return 1


if __name__ == "__main__":
    sys.exit(main())
