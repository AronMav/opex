/**
 * Fix M: a multi-agent shared session has TWO cache entries under the same
 * 3-element prefix `["sessions", id, "messages"]` — one per agent, keyed by a
 * 4th `agent` element. getCachedHistoryMessages / getCachedRawMessages must
 * pick THIS agent's entry, not `results[0]` (insertion order), which could be
 * the other agent's stale snapshot → stale render + wrong leaf_message_id.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";
import { getCachedRawMessages, getCachedHistoryMessages } from "@/stores/chat-history";
import type { MessageRow } from "@/types/api";

const SID = "shared-session";
const AGENT_A = "AgentA";
const AGENT_B = "AgentB";

function row(id: string, content: string): MessageRow {
  return {
    id,
    role: "assistant",
    content,
    tool_calls: null,
    tool_call_id: null,
    created_at: "2026-07-16T00:00:00Z",
    agent_id: null,
    feedback: null,
    edited_at: null,
    status: "done",
    thinking_blocks: null,
    parent_message_id: null,
    branch_from_message_id: null,
    abort_reason: null,
    is_mirror: false,
  } as unknown as MessageRow;
}

beforeEach(() => {
  queryClient.clear();
  // AgentA is inserted FIRST → it is results[0] under the shared prefix.
  queryClient.setQueryData([...qk.sessionMessages(SID), AGENT_A], { messages: [row("a-1", "from A")] });
  queryClient.setQueryData([...qk.sessionMessages(SID), AGENT_B], { messages: [row("b-1", "from B")] });
});

describe("Fix M — agent-scoped cache read", () => {
  it("getCachedRawMessages returns the requested agent's rows, not results[0]", () => {
    // Without the agent arg, legacy behaviour returns the first (AgentA) entry.
    expect(getCachedRawMessages(SID)[0]?.id).toBe("a-1");
    // With the agent arg it must return the matching entry.
    expect(getCachedRawMessages(SID, AGENT_B).map((r) => r.id)).toEqual(["b-1"]);
    expect(getCachedRawMessages(SID, AGENT_A).map((r) => r.id)).toEqual(["a-1"]);
  });

  it("getCachedHistoryMessages resolves the requested agent's content", () => {
    const bMsgs = getCachedHistoryMessages(SID, AGENT_B);
    expect(bMsgs.some((m) => m.id === "b-1")).toBe(true);
    expect(bMsgs.some((m) => m.id === "a-1")).toBe(false);
  });

  it("falls back to results[0] when the agent has no matching entry", () => {
    expect(getCachedRawMessages(SID, "AgentZ")[0]?.id).toBe("a-1");
  });
});
