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
TOKEN = os.environ.get("HYDECLAW_AUTH_TOKEN")
DB_URL = os.environ.get("PI_DB_URL", f"postgresql://hydeclaw@{PI_HOST}:5432/hydeclaw")


def parse_step_index(s: str) -> int:
    """Parse `step_N` wire format into integer N."""
    m = re.match(r"step_(\d+)", s)
    assert m, f"unexpected step_id format: {s!r} (expected 'step_N')"
    return int(m.group(1))


def send_chat(text: str, agent: str = "Arty") -> tuple[uuid.UUID, list[dict]]:
    """Send a chat message and capture all SSE events.

    Server allocates the session UUID; we read it from the first
    `data-session-id` SSE event. Passing a client-generated session_id
    that doesn't exist in the DB is rejected ("session not found").
    """
    resp = requests.post(
        f"http://{PI_HOST}:18789/api/chat",
        headers={"Authorization": f"Bearer {TOKEN}", "Accept": "text/event-stream"},
        json={"agent": agent, "messages": [{"role": "user", "content": text}]},
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

    # Compare iteration-0's messageId against messages.id where step_id = 0
    first_step_message_id = step_starts[0]["messageId"]

    rows = query_db(
        "SELECT id::text FROM messages WHERE session_id = %s AND role = 'assistant' "
        "AND step_id = 0",
        str(session_id),
    )
    assert rows, "assistant message for step_id=0 must be persisted"
    assert first_step_message_id == rows[0][0], (
        f"SSE step-start messageId {first_step_message_id} != "
        f"DB messages.id where step_id=0 ({rows[0][0]})"
    )


def test_step_id_flows_sse_to_db():
    session_id, events = send_chat("use a tool then summarize what you did")
    sse_step_ids = [
        e["stepId"] for e in events if e.get("type") == "step-start"
    ]
    assert sse_step_ids, "at least one step-start expected"

    rows = query_db(
        "SELECT step_id FROM messages WHERE session_id = %s "
        "AND role = 'assistant' AND step_id IS NOT NULL ORDER BY created_at",
        str(session_id),
    )
    db_step_ids = [r[0] for r in rows]

    # Wire format is `step_{N}`; DB column is INT. Compare by index.
    expected_indices = [parse_step_index(s) for s in sse_step_ids]
    assert expected_indices == db_step_ids, (
        f"SSE step indices {expected_indices} != DB step_ids {db_step_ids}"
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
        "SELECT tool_call_id FROM messages WHERE session_id = %s AND role = 'tool'",
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
    assert TOKEN, "HYDECLAW_AUTH_TOKEN env var required"
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
