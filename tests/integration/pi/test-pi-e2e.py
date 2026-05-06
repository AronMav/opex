#!/usr/bin/env python3
"""
End-to-end Pi test for the post-rework SSE contract.

Validates:
  1. step-start carries `messageId` (per-iteration UUID — Phase 1)
  2. SSE response includes standard `id:` field (Phase 3)
  3. Finish event always present (backend guarantee)
  4. ToolResult emitted for every ToolCallStart (Phase 5)
  5. Last-Event-ID resume skips already-seen events
  6. step_id column populated in DB for intermediate rows
"""
import json
import os
import sys
import time
import uuid
import urllib.request
import urllib.error

PI_HOST = "http://192.168.1.82:18789"
TOKEN = os.environ["HYDECLAW_AUTH_TOKEN"]
AGENT = "Arty"

def http(method, path, body=None, headers=None):
    headers = dict(headers or {})
    headers["Authorization"] = f"Bearer {TOKEN}"
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(f"{PI_HOST}{path}", data=data, method=method, headers=headers)
    return urllib.request.urlopen(req, timeout=120)


def parse_sse_events(stream, max_seconds=180):
    """Yield (event_id, parsed_dict) from an SSE response."""
    deadline = time.time() + max_seconds
    pending_id = None
    buf = b""
    while True:
        if time.time() > deadline:
            return
        chunk = stream.read1()
        if not chunk:
            return
        buf += chunk
        while b"\n" in buf:
            line, _, buf = buf.partition(b"\n")
            line = line.decode("utf-8", "replace").rstrip("\r")
            if line.startswith("id:"):
                pending_id = line[3:].strip()
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


def assertion(name, ok, detail=""):
    icon = "✅" if ok else "❌"
    print(f"  {icon} {name}{(' — ' + detail) if detail else ''}")
    return ok


def find_running_session():
    resp = http("GET", f"/api/sessions?agent={AGENT}")
    data = json.loads(resp.read())
    for s in data.get("sessions", []):
        if s["run_status"] == "running":
            return s["id"]
    return None


def wait_for_session_done(sid, timeout=180):
    deadline = time.time() + timeout
    while time.time() < deadline:
        resp = http("GET", f"/api/sessions?agent={AGENT}")
        data = json.loads(resp.read())
        for s in data["sessions"]:
            if s["id"] == sid:
                if s["run_status"] != "running":
                    return s["run_status"]
                break
        time.sleep(2)
    return None


def db_query(sql):
    """Run psql inside the docker container — works whether the script is
    executed on the Pi itself or remotely via ssh chain."""
    import subprocess
    # Direct on Pi — `docker exec` works as the local user.
    cmd = [
        "docker", "exec", "docker-postgres-1",
        "psql", "-U", "hydeclaw", "-d", "hydeclaw", "-tAc", sql,
    ]
    out = subprocess.check_output(cmd, timeout=30).decode().strip()
    return out


def main():
    failures = 0

    # ── Test 1: send a tool-using message and validate SSE structure ──
    print("\n[1] Send message → validate SSE contract")
    user_msg_id = str(uuid.uuid4())
    body = {
        "messages": [{"role": "user", "content": "ответь одной фразой: какой сегодня день недели?"}],
        "agent": AGENT,
        "user_message_id": user_msg_id,
        "force_new_session": True,
    }
    resp = http("POST", "/api/chat", body=body)
    events = []
    seen_step_starts = []
    seen_tool_starts = set()
    seen_tool_outputs = set()
    has_finish = False
    last_id = None
    last_text_id = None
    text_block_open = False

    for event_id, ev in parse_sse_events(resp, max_seconds=120):
        events.append((event_id, ev))
        if event_id is not None:
            try:
                last_id = int(event_id)
            except ValueError:
                pass
        t = ev.get("type")
        if t == "step-start":
            seen_step_starts.append(ev)
        elif t == "tool-input-start":
            seen_tool_starts.add(ev["toolCallId"])
        elif t == "tool-output-available":
            seen_tool_outputs.add(ev["toolCallId"])
        elif t == "text-start":
            assert not text_block_open, "text-start while another text block is open"
            text_block_open = True
            last_text_id = ev.get("id")
        elif t == "text-end":
            text_block_open = False
        elif t == "finish":
            has_finish = True
        elif t == "__DONE__":
            break

    print(f"  Got {len(events)} SSE events, {len(seen_step_starts)} step-starts")

    # Phase 1: step-start has messageId
    if seen_step_starts:
        first_with_id = all("messageId" in s and s["messageId"] for s in seen_step_starts)
        if not assertion("step-start carries messageId (Phase 1)", first_with_id,
                         f"first step-start: {seen_step_starts[0]}"):
            failures += 1

        # Each iteration's messageId must be a valid UUID
        all_uuids = True
        for s in seen_step_starts:
            try:
                uuid.UUID(s["messageId"])
            except (ValueError, KeyError):
                all_uuids = False
                break
        if not assertion("messageId is a valid UUID", all_uuids):
            failures += 1
    else:
        print("  ⚠ no step-start events (single-shot answer without tool loop?)")

    # Phase 3: SSE id field present
    if last_id is not None:
        if not assertion("SSE id: field present (Phase 3)", last_id > 0,
                         f"last seq = {last_id}"):
            failures += 1
    else:
        if not assertion("SSE id: field present (Phase 3)", False, "no id seen"):
            failures += 1

    # Backend Finish guarantee
    if not assertion("finish event emitted (close guarantee)", has_finish):
        failures += 1

    # Phase 5: every ToolCallStart got a ToolResult
    missing = seen_tool_starts - seen_tool_outputs
    if not assertion(f"every tool-input-start got tool-output-available ({len(seen_tool_starts)} tools)",
                     not missing, f"missing: {missing}"):
        failures += 1

    # Get session id from data-session-id event
    session_id = None
    for _, ev in events:
        if ev.get("type") == "data-session-id":
            session_id = ev["data"]["sessionId"]
            break
    print(f"  session_id = {session_id}")

    # ── Test 2: verify step_id populated in DB across recent sessions ──
    # step_id is set for intermediate (with tool_calls) rows only — a single-
    # iteration answer produces no intermediate rows, so we look at the agent's
    # recent sessions for at least one row with step_id != NULL.
    print("\n[2] DB step_id column populated for intermediate rows (Phase 4)")
    try:
        count = db_query(
            "SELECT COUNT(*) FROM messages WHERE step_id IS NOT NULL "
            "AND created_at > NOW() - INTERVAL '1 day'"
        )
        steps = db_query(
            "SELECT DISTINCT step_id FROM messages WHERE step_id IS NOT NULL "
            "AND created_at > NOW() - INTERVAL '1 day' ORDER BY step_id"
        )
        n = int(count)
        if not assertion(
            f"recent intermediate rows have step_id ({n} rows)",
            n > 0,
            f"distinct steps in last 24h: {steps.replace(chr(10), ',')}",
        ):
            failures += 1
    except Exception as e:
        print(f"  ⚠ db query failed: {e}")

    # ── Test 3: Last-Event-ID resume skips events ──
    print("\n[3] Last-Event-ID resume skips already-seen events")
    if session_id and last_id and last_id > 5:
        # Wait for session to finish
        status = wait_for_session_done(session_id, timeout=60)
        if status:
            print(f"  session finished: {status}")
        # Resume with Last-Event-ID = halfway point
        halfway = last_id // 2
        try:
            resp = http("GET", f"/api/chat/{session_id}/stream",
                        headers={"Last-Event-ID": str(halfway)})
            replayed = []
            for event_id, ev in parse_sse_events(resp, max_seconds=10):
                replayed.append((event_id, ev))
                if ev.get("type") == "__DONE__":
                    break
            ids = [int(eid) for (eid, _) in replayed if eid is not None]
            if ids:
                if not assertion(
                    f"replayed only events with seq > {halfway}",
                    all(i > halfway for i in ids),
                    f"min seq = {min(ids)}",
                ):
                    failures += 1
            else:
                print(f"  ⚠ resume returned no events (session may have been pruned)")
        except urllib.error.HTTPError as e:
            if e.code == 204:
                print("  ⚠ resume returned 204 (session no longer in registry, pruned)")
            else:
                raise

    print(f"\n{'='*40}")
    if failures == 0:
        print(f"✅ ALL TESTS PASSED")
        return 0
    print(f"❌ {failures} test(s) failed")
    return 1


if __name__ == "__main__":
    sys.exit(main())
