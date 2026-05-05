#!/usr/bin/env python3
"""
Chaos test: simulate SSE disconnects mid-stream and validate that
Last-Event-ID resume produces a complete, deduplicated event timeline.

Approach:
  1. POST /api/chat → start a tool-using stream
  2. After N events, hard-close the connection (chaos drop)
  3. Resume with Last-Event-ID = highest id seen so far
  4. Continue reading until [DONE]
  5. Concatenate event lists and assert:
     - no duplicate seq ids across drop boundary
     - finish event present
     - all step-start messageIds unique

Run on Pi where docker-postgres is reachable:
  HYDECLAW_AUTH_TOKEN=<token> python3 test-pi-chaos.py
"""
import json
import os
import random
import sys
import time
import urllib.request

PI = "http://192.168.1.82:18789"
TOKEN = os.environ["HYDECLAW_AUTH_TOKEN"]
AGENT = "Arty"


def parse_sse(stream):
    """Yield (id_int_or_None, parsed_dict) until stream closes."""
    buf = b""
    pending_id = None
    while True:
        chunk = stream.read1()
        if not chunk:
            return
        buf += chunk
        while b"\n" in buf:
            line, _, buf = buf.partition(b"\n")
            line = line.decode("utf-8", "replace").rstrip("\r")
            if line.startswith("id:"):
                try:
                    pending_id = int(line[3:].strip())
                except ValueError:
                    pending_id = None
            elif line.startswith("data:"):
                data = line[5:].strip()
                if data == "[DONE]":
                    yield (pending_id, {"type": "__DONE__"})
                    return
                try:
                    yield (pending_id, json.loads(data))
                except json.JSONDecodeError:
                    pass
                pending_id = None
            elif line == "":
                pending_id = None


def post_chat(prompt):
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
        },
    )
    return urllib.request.urlopen(req, timeout=60)


def resume(sid, last_id):
    req = urllib.request.Request(
        f"{PI}/api/chat/{sid}/stream",
        method="GET",
        headers={
            "Authorization": f"Bearer {TOKEN}",
            "Last-Event-ID": str(last_id),
        },
    )
    return urllib.request.urlopen(req, timeout=120)


def main():
    failures = 0
    print("=== Chaos test: random mid-stream drop + Last-Event-ID resume ===\n")

    drop_after = random.randint(3, 6)
    print(f"[plan] dropping connection after {drop_after} events received\n")

    sid = None
    pre_drop_events = []
    pre_drop_last_id = 0

    print("[1] Open initial stream")
    resp = post_chat("найди новости и составь короткий дайджест в одну фразу")
    for eid, ev in parse_sse(resp):
        pre_drop_events.append((eid, ev))
        if eid is not None:
            pre_drop_last_id = max(pre_drop_last_id, eid)
        if ev.get("type") == "data-session-id":
            sid = ev["data"]["sessionId"]
        if len(pre_drop_events) >= drop_after:
            break  # simulate drop
    resp.close()

    print(f"  session = {sid}")
    print(f"  saw {len(pre_drop_events)} events, last id = {pre_drop_last_id}")
    if not sid:
        print("❌ never got session id — abort")
        return 1

    # Wait for backend to keep producing
    time.sleep(2)

    print(f"\n[2] Resume from Last-Event-ID = {pre_drop_last_id}")
    post_drop_events = []
    post_drop_min_id = None
    finish_seen = False
    try:
        resp2 = resume(sid, pre_drop_last_id)
        for eid, ev in parse_sse(resp2):
            post_drop_events.append((eid, ev))
            if eid is not None:
                if post_drop_min_id is None:
                    post_drop_min_id = eid
                else:
                    post_drop_min_id = min(post_drop_min_id, eid)
            if ev.get("type") == "finish":
                finish_seen = True
            if ev.get("type") == "__DONE__":
                break
        resp2.close()
    except urllib.error.HTTPError as e:
        if e.code == 204:
            print("  resume returned 204 — backend already finalized; OK")
            finish_seen = True  # no events to dedup
        else:
            raise
    print(f"  resume returned {len(post_drop_events)} events, min id = {post_drop_min_id}")

    print("\n--- Invariants ---")

    # 1. All resume seq ids strictly greater than pre_drop_last_id
    if post_drop_min_id is not None:
        if post_drop_min_id > pre_drop_last_id:
            print(f"✅ resume strictly skipped already-seen events ({post_drop_min_id} > {pre_drop_last_id})")
        else:
            print(f"❌ resume returned seq <= last_id ({post_drop_min_id} <= {pre_drop_last_id})")
            failures += 1

    # 2. Combined timeline contains finish
    if finish_seen:
        print("✅ stream eventually closed with finish (or 204)")
    else:
        print("❌ no finish event after resume — stream never closed cleanly")
        failures += 1

    # 3. No duplicate event seq ids across the boundary
    pre_ids = {eid for eid, _ in pre_drop_events if eid is not None}
    post_ids = {eid for eid, _ in post_drop_events if eid is not None}
    overlap = pre_ids & post_ids
    if not overlap:
        print(f"✅ no duplicate seq ids across drop boundary "
              f"({len(pre_ids)} pre + {len(post_ids)} post)")
    else:
        print(f"❌ duplicate seq ids across drop: {sorted(overlap)[:10]}")
        failures += 1

    # 4. step-start messageIds unique
    step_ids = []
    for eid, ev in pre_drop_events + post_drop_events:
        if ev.get("type") == "step-start":
            step_ids.append(ev.get("messageId"))
    duplicate_steps = [s for s in set(step_ids) if step_ids.count(s) > 1]
    if not duplicate_steps:
        print(f"✅ all {len(step_ids)} step-start messageIds unique")
    else:
        print(f"❌ duplicate step-start messageIds: {duplicate_steps}")
        failures += 1

    print("\n" + "=" * 40)
    if failures == 0:
        print("✅ CHAOS TEST PASSED — Last-Event-ID resume is robust")
        return 0
    print(f"❌ {failures} invariant(s) violated")
    return 1


if __name__ == "__main__":
    sys.exit(main())
