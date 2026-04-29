import { describe, it, expect } from "vitest";
import { convertHistory, resolveActivePath } from "@/stores/chat-history";
import type { MessageRow } from "@/types/api";

// Helper to build a MessageRow with sensible defaults; only overrides need
// to be spelled out per test case.
function makeRow(overrides: Partial<MessageRow> & { id: string; created_at?: string }): MessageRow {
  return {
    role: "assistant",
    content: "",
    tool_calls: null,
    tool_call_id: null,
    created_at: overrides.created_at ?? new Date().toISOString(),
    agent_id: null,
    status: "complete",
    feedback: 0,
    edited_at: null,
    parent_message_id: null,
    branch_from_message_id: null,
    abort_reason: null,
    thinking_blocks: null,
    ...overrides,
  };
}

describe("convertHistory — streaming placeholder does not shadow tree root (Bug 2)", () => {
  it("resolveActivePath_ignores_NULL_parent_streaming_row", () => {
    // Arrange a conversation where a streaming placeholder row with
    // parent_message_id=null appears AFTER the real user/assistant/user
    // chain. Today (pre-fix) resolveActivePath picks the streaming
    // placeholder as roots[0] and drops the real tree, so the
    // second user message "follow-up" disappears from the rendered path.
    const rows: MessageRow[] = [
      makeRow({
        id: "u1",
        role: "user",
        parent_message_id: null,
        status: "complete",
        content: "hi",
        created_at: "2026-04-20T10:00:00Z",
      }),
      makeRow({
        id: "a1",
        role: "assistant",
        parent_message_id: "u1",
        status: "complete",
        content: "hello",
        agent_id: "Arty",
        created_at: "2026-04-20T10:00:01Z",
      }),
      makeRow({
        id: "u2",
        role: "user",
        parent_message_id: "a1",
        status: "complete",
        content: "follow-up",
        created_at: "2026-04-20T10:00:02Z",
      }),
      // The villain: a streaming placeholder with NULL parent and later
      // created_at. Under the pre-fix convertHistory this row becomes
      // the second root, but roots.sort() + roots[0] picks u1 anyway —
      // UNLESS the placeholder is ordered earlier. To make the shadow
      // reproducible we set its created_at BEFORE u1 so resolveActivePath
      // picks it and walks no children.
      makeRow({
        id: "s1",
        role: "assistant",
        parent_message_id: null,
        status: "streaming",
        content: "",
        agent_id: "Arty",
        created_at: "2026-04-20T09:59:59Z",
      }),
    ];

    // selectedBranches is empty — the rendered path is just "walk newest child".
    const messages = convertHistory(rows, true, {});

    // Expected: streaming placeholder is filtered out BEFORE resolveActivePath,
    // so u1 is the sole root, and the walk reaches a1 → u2.
    const roles = messages.map(m => m.role);
    expect(roles).toContain("user");
    // The regression gate: the "follow-up" user message MUST be present.
    const userContents = messages
      .filter(m => m.role === "user")
      .flatMap(m => m.parts)
      .filter((p): p is { type: "text"; text: string } => p.type === "text")
      .map(p => p.text);
    expect(userContents).toContain("follow-up");

    // And: no streaming placeholder leaks into the output.
    const ids = messages.map(m => m.id);
    expect(ids).not.toContain("s1");

    // Sanity: the real chain is in render order.
    expect(ids).toEqual(["u1", "a1", "u2"]);
  });

  it("sensitivity probe: resolveActivePath picks the NULL-parent streaming row as root if filter runs AFTER", () => {
    // Informational probe — proves the bug is real. If we call
    // resolveActivePath DIRECTLY on rows (no pre-filter), the streaming
    // placeholder shadows u1 because its created_at is earlier.
    const rows: MessageRow[] = [
      makeRow({
        id: "u1",
        role: "user",
        parent_message_id: null,
        status: "complete",
        content: "hi",
        created_at: "2026-04-20T10:00:00Z",
      }),
      makeRow({
        id: "a1",
        role: "assistant",
        parent_message_id: "u1",
        status: "complete",
        content: "hello",
        created_at: "2026-04-20T10:00:01Z",
      }),
      makeRow({
        id: "u2",
        role: "user",
        parent_message_id: "a1",
        status: "complete",
        content: "follow-up",
        created_at: "2026-04-20T10:00:02Z",
      }),
      makeRow({
        id: "s1",
        role: "assistant",
        parent_message_id: null,
        status: "streaming",
        content: "",
        created_at: "2026-04-20T09:59:59Z",
      }),
    ];

    const path = resolveActivePath(rows, {});
    const ids = path.map(r => r.id);
    // Bug shape: s1 is the earliest root → path is just [s1] with no children.
    // If convertHistory ran resolveActivePath FIRST then filtered streaming
    // rows AFTER, u1/a1/u2 would be lost. This probe guarantees the pre-fix
    // order is demonstrably broken.
    expect(ids[0]).toBe("s1");
    expect(ids).not.toContain("u2");
  });
});

describe("convertHistory — parallel tool results sorted by declared order", () => {
  // Parallel tool calls complete fastest-first, so DB insertion order does NOT
  // match the declared order in tool_calls[]. convertHistory must sort tool
  // parts within each consecutive group back into declared order.
  //
  // Scenario: assistant declares [agents_list(call_00), search(call_01), search(call_02)]
  // but results arrive in DB order: call_01 first, call_02 second, call_00 last.

  it("sorts consecutive tool parts by declared tool_calls[] index, not DB arrival order", () => {
    const rows: MessageRow[] = [
      makeRow({
        id: "u1",
        role: "user",
        content: "Discuss world in 3 cycles",
        created_at: "2026-04-29T10:50:04Z",
      }),
      // Assistant declaring [agents_list(call_00), search(call_01), search(call_02)]
      makeRow({
        id: "a1",
        role: "assistant",
        agent_id: "Arty",
        content: "",
        tool_calls: [
          { id: "call_00_agents", name: "agents_list",    arguments: {} },
          { id: "call_01_search", name: "search_web_fresh", arguments: {} },
          { id: "call_02_search", name: "search_web_fresh", arguments: {} },
        ] as any,
        created_at: "2026-04-29T10:50:15Z",
      }),
      // Tool results arrive in completion order: call_01 first (fastest), call_02, call_00 last
      makeRow({ id: "t1", role: "tool", tool_call_id: "call_01_search", content: "search result 1", created_at: "2026-04-29T10:50:17.662Z" }),
      makeRow({ id: "t2", role: "tool", tool_call_id: "call_02_search", content: "search result 2", created_at: "2026-04-29T10:50:17.676Z" }),
      makeRow({ id: "t3", role: "tool", tool_call_id: "call_00_agents", content: "agents result",   created_at: "2026-04-29T10:50:17.800Z" }),
    ];

    const messages = convertHistory(rows, false, {});

    // Find the assistant message
    const assistant = messages.find(m => m.role === "assistant");
    expect(assistant).toBeDefined();

    const toolParts = assistant!.parts.filter(p => p.type === "tool") as Array<{ type: "tool"; toolCallId: string; toolName: string }>;

    expect(toolParts).toHaveLength(3);

    // DECLARED ORDER: agents_list(call_00) first, search(call_01) second, search(call_02) third
    expect(toolParts[0].toolCallId).toBe("call_00_agents");
    expect(toolParts[0].toolName).toBe("agents_list");
    expect(toolParts[1].toolCallId).toBe("call_01_search");
    expect(toolParts[2].toolCallId).toBe("call_02_search");
  });

  it("does not reorder tool parts from different iterations (text separator between groups)", () => {
    // Iteration 1: search x2; Iteration 2: agent (separated by text "Analyzing...")
    // Tools from different iterations must NOT be mixed when sorting.
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "Hello", created_at: "2026-04-29T10:00:00Z" }),
      makeRow({
        id: "a1",
        role: "assistant",
        agent_id: "Arty",
        content: "",
        tool_calls: [
          { id: "call_00_s", name: "search", arguments: {} },
          { id: "call_01_s", name: "search", arguments: {} },
        ] as any,
        created_at: "2026-04-29T10:00:01Z",
      }),
      // Results arrive in reverse declared order
      makeRow({ id: "t1", role: "tool", tool_call_id: "call_01_s", content: "r1", created_at: "2026-04-29T10:00:02Z" }),
      makeRow({ id: "t2", role: "tool", tool_call_id: "call_00_s", content: "r2", created_at: "2026-04-29T10:00:03Z" }),
      // Second iteration (merged into same bubble by virtual merging)
      makeRow({
        id: "a2",
        role: "assistant",
        agent_id: "Arty",
        content: "Analyzing...",
        tool_calls: [{ id: "call_00_a", name: "agent", arguments: {} }] as any,
        created_at: "2026-04-29T10:00:04Z",
      }),
      makeRow({ id: "t3", role: "tool", tool_call_id: "call_00_a", content: "agent result", created_at: "2026-04-29T10:00:05Z" }),
    ];

    const messages = convertHistory(rows, false, {});
    const assistant = messages.find(m => m.role === "assistant");
    expect(assistant).toBeDefined();

    const toolParts = assistant!.parts.filter(p => p.type === "tool") as Array<{ type: "tool"; toolCallId: string }>;
    // Iteration 1 group: call_00_s FIRST (declared index 0), call_01_s second (declared index 1)
    expect(toolParts[0].toolCallId).toBe("call_00_s");
    expect(toolParts[1].toolCallId).toBe("call_01_s");
    // Iteration 2 group: separated by text "Analyzing..." → agent tool last
    expect(toolParts[2].toolCallId).toBe("call_00_a");
  });
});
