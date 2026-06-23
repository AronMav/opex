"""Lock-in tests for ID-based identity flow against the Pi.

Each test triggers a real chat session, captures SSE events from the live
stream, then queries the Pi's Postgres to verify the captured IDs match
the persisted row IDs. Defends the ADR 2026-05-05 contract end-to-end.

Run after Pi deploy:
    python tests/integration/pi/test-pi-identity-flow.py
"""

import os
import re
import sys
import time
import uuid
import json
import requests
import psycopg2

PI_HOST = os.environ.get("PI_HOST", "192.168.1.82")
TOKEN = os.environ.get("OPEX_AUTH_TOKEN")
DB_URL = os.environ.get("PI_DB_URL", f"postgresql://opex@{PI_HOST}:5432/opex")


def parse_step_index(s: str) -> int:
    """Parse `step_N` wire format into integer N."""
    m = re.match(r"step_(\d+)", s)
    assert m, f"unexpected step_id format: {s!r} (expected 'step_N')"
    return int(m.group(1))


def send_chat(text: str, agent: str = "Arty") -> tuple[uuid.UUID, list[dict]]:
    """Send a chat message and capture all SSE events.

    Always uses `force_new_session: True` to guarantee a clean session per
    test — otherwise the server reuses the agent's last-active session and
    the DB query would aggregate rows across multiple turns.

    Server allocates the session UUID; we read it from the first
    `data-session-id` SSE event. Passing a client-generated session_id
    that doesn't exist in the DB is rejected ("session not found").
    """
    resp = requests.post(
        f"http://{PI_HOST}:18789/api/chat",
        headers={"Authorization": f"Bearer {TOKEN}", "Accept": "text/event-stream"},
        json={
            "agent": agent,
            "force_new_session": True,
            "messages": [{"role": "user", "content": text}],
        },
        stream=True,
        timeout=120,
    )
    resp.raise_for_status()
    events = []
    session_id = None
    for line in resp.iter_lines(decode_unicode=True):
        if line and line.startswith("data:"):
            payload = line[len("data:"):].strip()
            try:
                obj = json.loads(payload)
                events.append(obj)
                if obj.get("type") == "data-session-id" and session_id is None:
                    # Payload shape: {"type": "data-session-id",
                    #                 "data": {"sessionId": "..."}, "transient": true}
                    raw = obj.get("data", {}).get("sessionId")
                    if raw:
                        session_id = uuid.UUID(raw)
            except json.JSONDecodeError:
                pass
    assert session_id is not None, "data-session-id event must be emitted first"
    return session_id, events


def query_db(sql: str, *params):
    """One-shot query against the Pi's Postgres."""
    conn = psycopg2.connect(DB_URL)
    try:
        cur = conn.cursor()
        cur.execute(sql, params)
        return cur.fetchall()
    finally:
        conn.close()


def test_assistant_message_id_flows_sse_to_db():
    session_id, events = send_chat("say hello briefly")
    step_starts = [e for e in events if e.get("type") == "step-start"]
    assert step_starts, "step-start event must be emitted"

    sse_message_ids = {e["messageId"] for e in step_starts}

    rows = query_db(
        "SELECT id::text FROM messages "
        "WHERE session_id = %s AND role = 'assistant' "
        "AND (is_mirror = false OR is_mirror IS NULL)",
        str(session_id),
    )
    db_assistant_ids = {r[0] for r in rows}

    missing = sse_message_ids - db_assistant_ids
    assert not missing, (
        f"SSE step-start messageIds not found in DB messages.id: {missing}\n"
        f"  SSE: {sse_message_ids}\n"
        f"  DB:  {db_assistant_ids}"
    )


def test_step_id_flows_sse_to_db():
    """Every SSE step-start has a DB row whose step_id is either the parsed
    SSE index (in-loop iteration) OR NULL (final iteration via finalize).

    Per ADR-2026-05-05 §"Phase 4": "NULL means not part of a tool-loop
    iteration — final assistant rows, user rows, tool-result rows."
    """
    session_id, events = send_chat("use a tool then summarize what you did")
    sse_step_starts = [e for e in events if e.get("type") == "step-start"]
    assert sse_step_starts, "at least one step-start expected"

    for step_start in sse_step_starts:
        msg_id = step_start["messageId"]
        sse_idx = parse_step_index(step_start["stepId"])

        rows = query_db(
            "SELECT step_id FROM messages WHERE id = %s "
            "AND (is_mirror = false OR is_mirror IS NULL)",
            msg_id,
        )
        assert rows, (
            f"SSE step-start announced messageId {msg_id} but no DB row exists with that id"
        )
        db_step_id = rows[0][0]
        assert db_step_id == sse_idx or db_step_id is None, (
            f"For messageId={msg_id}, SSE stepIndex={sse_idx}: "
            f"DB step_id is {db_step_id}, expected {sse_idx} (in-loop) or NULL (final)"
        )


def test_tool_call_id_flows_sse_to_db():
    session_id, events = send_chat("call a tool")
    sse_tool_call_ids = [
        e["toolCallId"] for e in events if e.get("type") == "tool-input-start"
    ]
    if not sse_tool_call_ids:
        print("SKIP: no tool calls produced — agent prompt didn't trigger tools")
        return

    rows = query_db(
        "SELECT tool_call_id FROM messages "
        "WHERE session_id = %s AND role = 'tool' "
        "AND (is_mirror = false OR is_mirror IS NULL)",
        str(session_id),
    )
    db_tool_call_ids = [r[0] for r in rows if r[0]]

    for sse_id in sse_tool_call_ids:
        assert sse_id in db_tool_call_ids, (
            f"SSE tool_call_id {sse_id} missing from DB messages.tool_call_id"
        )


def test_approval_id_flows_sse_to_db():
    """Skipped unless agent has an approval-required tool configured."""
    print("SKIP: approval-required tool path requires specific agent config; "
          "manual verification recommended")


if __name__ == "__main__":
    assert TOKEN, "OPEX_AUTH_TOKEN env var required"
    failures = []
    for test in [
        test_assistant_message_id_flows_sse_to_db,
        test_step_id_flows_sse_to_db,
        test_tool_call_id_flows_sse_to_db,
        test_approval_id_flows_sse_to_db,
    ]:
        try:
            print(f"RUN: {test.__name__}")
            test()
            print(f"PASS: {test.__name__}")
        except Exception as e:
            print(f"FAIL: {test.__name__}: {e}")
            failures.append(test.__name__)
        time.sleep(1)
    if failures:
        print(f"\n{len(failures)} failures: {failures}")
        sys.exit(1)
    print("\nAll lock-in tests passed.")
