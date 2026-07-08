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
    is_mirror: false,
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

  it("sensitivity probe: resolveActivePath on unfiltered rows returns all rows sorted by created_at (streaming placeholder first)", () => {
    // Post-D1 informational probe — proves that convertHistory must still
    // pre-filter streaming rows BEFORE calling resolveActivePath.
    // After D1, with no branch_from_message_id, resolveActivePath short-circuits
    // to a chronological sort instead of a tree walk. All rows (including the
    // streaming placeholder) are returned, with the earliest first.
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
    // Post-D1 shape: no branch_from_message_id → fast sort path.
    // s1 is earliest (09:59:59) so it appears first; all rows are present.
    // This proves convertHistory must pre-filter streaming rows before calling
    // resolveActivePath to prevent the placeholder from leaking into output.
    expect(ids[0]).toBe("s1");
    expect(ids).toContain("u2");
    expect(ids).toEqual(["s1", "u1", "a1", "u2"]);
  });
});

describe("resolveActivePath — parallel tool siblings (all share same parent)", () => {
  it("includes all sibling tool messages when a node has multiple tool children", () => {
    // After the parallel.rs fix, all parallel tool results share the same parent
    // (the assistant message). resolveActivePath must include ALL of them, not
    // just the last child.
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", parent_message_id: null, created_at: "2026-04-30T10:00:00Z" }),
      makeRow({ id: "a1", role: "assistant", parent_message_id: "u1", created_at: "2026-04-30T10:00:01Z" }),
      // Three parallel tool results all share parent=a1
      makeRow({ id: "t1", role: "tool", parent_message_id: "a1", tool_call_id: "c1", created_at: "2026-04-30T10:00:02Z" }),
      makeRow({ id: "t2", role: "tool", parent_message_id: "a1", tool_call_id: "c2", created_at: "2026-04-30T10:00:03Z" }),
      makeRow({ id: "t3", role: "tool", parent_message_id: "a1", tool_call_id: "c3", created_at: "2026-04-30T10:00:04Z" }),
      // Next assistant chains off the last tool (t3)
      makeRow({ id: "a2", role: "assistant", parent_message_id: "t3", created_at: "2026-04-30T10:00:05Z" }),
    ];

    const path = resolveActivePath(rows, {});
    const ids = path.map(r => r.id);
    // All three tools must be in the path
    expect(ids).toEqual(["u1", "a1", "t1", "t2", "t3", "a2"]);
  });

  it("single tool child still works (sequential case unchanged)", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", parent_message_id: null, created_at: "2026-04-30T10:00:00Z" }),
      makeRow({ id: "a1", role: "assistant", parent_message_id: "u1", created_at: "2026-04-30T10:00:01Z" }),
      makeRow({ id: "t1", role: "tool", parent_message_id: "a1", tool_call_id: "c1", created_at: "2026-04-30T10:00:02Z" }),
      makeRow({ id: "a2", role: "assistant", parent_message_id: "t1", created_at: "2026-04-30T10:00:03Z" }),
    ];

    const path = resolveActivePath(rows, {});
    expect(path.map(r => r.id)).toEqual(["u1", "a1", "t1", "a2"]);
  });
});

describe("convertHistory — global tool index prevents mis-sort when intermediate assistant has no text", () => {
  it("orders tool results correctly when assistant[n] has no text content (no text separator in parts)", () => {
    // assistant[a1] makes call_first, tool[t1] returns error.
    // assistant[a2] retries with call_second, NO TEXT — so t2 lands in the
    // same consecutive tool-part run as t1. Without global index, both calls
    // have per-message idx=0 and sort is undefined.
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", parent_message_id: null, content: "hello", created_at: "2026-04-30T10:00:00Z" }),
      makeRow({
        id: "a1", role: "assistant", agent_id: "Arty", parent_message_id: "u1",
        content: "Starting...",
        tool_calls: [{ id: "call_first", name: "search", arguments: {} }] as any,
        created_at: "2026-04-30T10:00:01Z",
      }),
      makeRow({
        id: "t1", role: "tool", parent_message_id: "a1", tool_call_id: "call_first",
        content: "Error: bad query", created_at: "2026-04-30T10:00:02Z"
      }),
      // No text in a2 — its merge adds no text separator, so t2 joins t1's run
      makeRow({
        id: "a2", role: "assistant", agent_id: "Arty", parent_message_id: "t1",
        content: "",
        tool_calls: [{ id: "call_second", name: "search", arguments: {} }] as any,
        created_at: "2026-04-30T10:00:03Z",
      }),
      makeRow({
        id: "t2", role: "tool", parent_message_id: "a2", tool_call_id: "call_second",
        content: "Good result", created_at: "2026-04-30T10:00:04Z"
      }),
    ];

    const messages = convertHistory(rows, false, {});
    const assistant = messages.find(m => m.role === "assistant");
    expect(assistant).toBeDefined();
    const toolParts = assistant!.parts.filter(p => p.type === "tool") as Array<{ type: "tool"; toolCallId: string }>;
    // call_first (global=0) must appear before call_second (global=1)
    expect(toolParts).toHaveLength(2);
    expect(toolParts[0].toolCallId).toBe("call_first");
    expect(toolParts[1].toolCallId).toBe("call_second");
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
          { id: "call_00_agents", name: "agents_list", arguments: {} },
          { id: "call_01_search", name: "search_web", arguments: {} },
          { id: "call_02_search", name: "search_web", arguments: {} },
        ] as any,
        created_at: "2026-04-29T10:50:15Z",
      }),
      // Tool results arrive in completion order: call_01 first (fastest), call_02, call_00 last
      makeRow({ id: "t1", role: "tool", tool_call_id: "call_01_search", content: "search result 1", created_at: "2026-04-29T10:50:17.662Z" }),
      makeRow({ id: "t2", role: "tool", tool_call_id: "call_02_search", content: "search result 2", created_at: "2026-04-29T10:50:17.676Z" }),
      makeRow({ id: "t3", role: "tool", tool_call_id: "call_00_agents", content: "agents result", created_at: "2026-04-29T10:50:17.800Z" }),
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

describe("resolveActivePath — parallel batch heir picked by descendants, not created_at", () => {
  it("continues through the sibling whose id is parent_message_id of a later message, even if it is not the latest by created_at", () => {
    // Reproduces Arty's session 48fc271b-ebdc-4db0-8542-525a1c792cc6 step 1:
    //   - assistant a1 dispatches 3 parallel tools t_opex, t_alma, t_search
    //   - all three share parent_message_id = a1
    //   - backend's chain_parent advances to declaration-last parallel tool = t_search
    //   - sequential follow-up t_exch has parent_message_id = t_search
    //   - chain continues t_exch → a_final
    // The bug: walker picks heir = last-by-created_at = t_alma, which has no
    // descendants, so the walker dead-ends and t_exch, a_final are lost.
    // NOTE: `a_final` carries a sentinel `branch_from_message_id` to force the
    // walker to run after D1 lands (Task 4). Without it, D1 would short-circuit
    // this fixture to a created_at sort and bypass the D2 swap entirely — the
    // test would still pass but for the wrong reason. The sentinel value never
    // matches any real id, so it has no effect on traversal.
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", parent_message_id: null, created_at: "2026-05-13T10:30:00Z" }),
      makeRow({ id: "a1", role: "assistant", parent_message_id: "u1", created_at: "2026-05-13T10:31:35Z" }),
      // Parallel batch — all parent=a1, but t_alma is the LATEST by created_at
      // while t_search is the actual heir (has the descendant t_exch).
      makeRow({ id: "t_opex", role: "tool", parent_message_id: "a1", tool_call_id: "call_h", created_at: "2026-05-13T10:33:56.067Z" }),
      makeRow({ id: "t_search", role: "tool", parent_message_id: "a1", tool_call_id: "call_s", created_at: "2026-05-13T10:33:56.074Z" }),
      makeRow({ id: "t_alma", role: "tool", parent_message_id: "a1", tool_call_id: "call_a", created_at: "2026-05-13T10:33:56.090Z" }),
      // Sequential follow-up chains off t_search (declaration-last parallel tool).
      makeRow({ id: "t_exch", role: "tool", parent_message_id: "t_search", tool_call_id: "call_e", created_at: "2026-05-13T10:33:57.762Z" }),
      makeRow({ id: "a_final", role: "assistant", parent_message_id: "t_exch", branch_from_message_id: "sentinel-forces-walker", content: "final answer", created_at: "2026-05-13T10:35:26Z" }),
    ];

    const path = resolveActivePath(rows, {});
    const ids = path.map(r => r.id);

    // All 7 rows must be reachable, in walk order.
    // Parallel batch is included in created_at order with heir last,
    // then walker continues through heir.
    expect(ids).toEqual(["u1", "a1", "t_opex", "t_alma", "t_search", "t_exch", "a_final"]);
  });
});

describe("resolveActivePath — trunk-only conversation bypasses tree walk (D1)", () => {
  it("returns all messages sorted by created_at when branch_from_message_id is NULL everywhere, even with a parallel batch present", () => {
    // Same parallel-batch shape as Task 1 but with branch_from_message_id NULL
    // on every row (the realistic case — most sessions are never explicitly
    // forked). D1 short-circuit must fire and return rows in created_at order
    // — which differs from walker order because the D2 swap moves t_search
    // (heir) to the last slot, producing [..., t_opex, t_alma, t_search, ...]
    // in walker order vs [..., t_opex, t_search, t_alma, ...] in created_at
    // order. Distinct expected arrays prove the short-circuit actually fires.
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", parent_message_id: null, branch_from_message_id: null, created_at: "2026-05-13T10:30:00Z" }),
      makeRow({ id: "a1", role: "assistant", parent_message_id: "u1", branch_from_message_id: null, created_at: "2026-05-13T10:31:35Z" }),
      makeRow({ id: "t_opex", role: "tool", parent_message_id: "a1", branch_from_message_id: null, tool_call_id: "call_h", created_at: "2026-05-13T10:33:56.067Z" }),
      makeRow({ id: "t_search", role: "tool", parent_message_id: "a1", branch_from_message_id: null, tool_call_id: "call_s", created_at: "2026-05-13T10:33:56.074Z" }),
      makeRow({ id: "t_alma", role: "tool", parent_message_id: "a1", branch_from_message_id: null, tool_call_id: "call_a", created_at: "2026-05-13T10:33:56.090Z" }),
      makeRow({ id: "t_exch", role: "tool", parent_message_id: "t_search", branch_from_message_id: null, tool_call_id: "call_e", created_at: "2026-05-13T10:33:57.762Z" }),
      makeRow({ id: "a_final", role: "assistant", parent_message_id: "t_exch", branch_from_message_id: null, content: "final", created_at: "2026-05-13T10:35:26Z" }),
    ];

    const path = resolveActivePath(rows, {});
    const ids = path.map(r => r.id);
    // Pure created_at sort — t_search BEFORE t_alma (D2 swap not applied).
    expect(ids).toEqual(["u1", "a1", "t_opex", "t_search", "t_alma", "t_exch", "a_final"]);
  });
});

describe("resolveActivePath — parallel batch with no descendants is a real leaf", () => {
  it("includes all sibling tool messages and stops when none have descendants", () => {
    // If a session ends after a parallel batch (no continuation), the walker
    // must include every sibling in the path and then stop — not continue
    // through a phantom heir.
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", parent_message_id: null, branch_from_message_id: "FAKE", created_at: "2026-05-13T10:00:00Z" }),
      makeRow({ id: "a1", role: "assistant", parent_message_id: "u1", branch_from_message_id: null, created_at: "2026-05-13T10:00:01Z" }),
      makeRow({ id: "t1", role: "tool", parent_message_id: "a1", branch_from_message_id: null, tool_call_id: "c1", created_at: "2026-05-13T10:00:02Z" }),
      makeRow({ id: "t2", role: "tool", parent_message_id: "a1", branch_from_message_id: null, tool_call_id: "c2", created_at: "2026-05-13T10:00:03Z" }),
      makeRow({ id: "t3", role: "tool", parent_message_id: "a1", branch_from_message_id: null, tool_call_id: "c3", created_at: "2026-05-13T10:00:04Z" }),
    ];

    // Force walker by faking a branch on u1 (otherwise D1 short-circuits).
    const path = resolveActivePath(rows, {});
    expect(path.map(r => r.id)).toEqual(["u1", "a1", "t1", "t2", "t3"]);
  });
});

describe("resolveActivePath — branched session honors selectedBranches AND D2 swap (D1 + D2 together)", () => {
  it("walks the selected branch at the fork, excludes the unselected branch, and applies D2 swap inside the chosen subtree's parallel batch", () => {
    // u1 → a1 → u2 → [a2_main OR a2_alt (branched from a2_main)]
    // selectedBranches picks a2_alt; walker must follow that branch and
    // exclude a2_main. a2_alt's subtree contains a parallel batch where
    // the heir (t_mid) is not the last sibling by created_at, exercising
    // both D1 (branch_from triggers walker) and D2 (heir swap) in one path.
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", parent_message_id: null, created_at: "2026-05-13T10:00:00Z" }),
      makeRow({ id: "a1", role: "assistant", parent_message_id: "u1", created_at: "2026-05-13T10:00:01Z" }),
      makeRow({ id: "u2", role: "user", parent_message_id: "a1", created_at: "2026-05-13T10:00:02Z" }),
      makeRow({ id: "a2_main", role: "assistant", parent_message_id: "u2", content: "original", created_at: "2026-05-13T10:00:03Z" }),
      makeRow({ id: "a2_alt", role: "assistant", parent_message_id: "u2", branch_from_message_id: "a2_main", content: "alternative", created_at: "2026-05-13T10:00:04Z" }),
      // Parallel batch under a2_alt — heir is t_mid (not last by time).
      makeRow({ id: "t_first", role: "tool", parent_message_id: "a2_alt", tool_call_id: "c1", created_at: "2026-05-13T10:00:05Z" }),
      makeRow({ id: "t_mid", role: "tool", parent_message_id: "a2_alt", tool_call_id: "c2", created_at: "2026-05-13T10:00:06Z" }),
      makeRow({ id: "t_last", role: "tool", parent_message_id: "a2_alt", tool_call_id: "c3", created_at: "2026-05-13T10:00:07Z" }),
      makeRow({ id: "a_end", role: "assistant", parent_message_id: "t_mid", content: "done", created_at: "2026-05-13T10:00:08Z" }),
    ];

    const path = resolveActivePath(rows, { u2: "a2_alt" });
    const ids = path.map(r => r.id);
    // Walker picks a2_alt at the u2 fork; includes all parallel siblings;
    // continues through t_mid (the heir, swapped into last slot) to a_end.
    expect(ids).toEqual(["u1", "a1", "u2", "a2_alt", "t_first", "t_last", "t_mid", "a_end"]);
    // The unselected branch (a2_main) must NOT appear.
    expect(ids).not.toContain("a2_main");
  });
});

describe("resolveActivePath — degenerate two-heirs case (defensive doc)", () => {
  it("when two parallel siblings both have descendants, walker picks the first by created_at and the other subtree becomes unreachable", () => {
    // Defensive documentation test: the current backend (parallel.rs) only
    // chains `chain_parent` off a single sibling (`parallel_indices.last()`),
    // so this two-heirs state is NOT producible by correct backend code.
    // If a future backend change ever creates this shape (or DB corruption
    // does), the UI walker's `findIndex` picks the first heir by created_at.
    // The second heir's subtree is silently lost.
    //
    // We pin this behavior here so a future change to `resolveActivePath`
    // can't accidentally introduce hidden divergence without updating this
    // expectation. If you find this test failing, decide explicitly: either
    // (a) the new walker behavior is intentional and update this expectation,
    // or (b) the new walker shouldn't change in this corner.
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", parent_message_id: null, branch_from_message_id: "sentinel", created_at: "2026-05-13T10:00:00Z" }),
      makeRow({ id: "a1", role: "assistant", parent_message_id: "u1", created_at: "2026-05-13T10:00:01Z" }),
      // Two tool siblings of a1 — t_one earlier by created_at, t_two later.
      makeRow({ id: "t_one", role: "tool", parent_message_id: "a1", tool_call_id: "c1", created_at: "2026-05-13T10:00:02Z" }),
      makeRow({ id: "t_two", role: "tool", parent_message_id: "a1", tool_call_id: "c2", created_at: "2026-05-13T10:00:03Z" }),
      // BOTH t_one and t_two have descendants — degenerate corrupt state.
      makeRow({ id: "leaf_one", role: "assistant", parent_message_id: "t_one", content: "one's child", created_at: "2026-05-13T10:00:04Z" }),
      makeRow({ id: "leaf_two", role: "assistant", parent_message_id: "t_two", content: "two's child (unreachable)", created_at: "2026-05-13T10:00:05Z" }),
    ];

    const path = resolveActivePath(rows, {});
    const ids = path.map(r => r.id);
    // `findIndex` finds t_one first (earlier created_at). Walker swaps it to
    // last position (no-op here since length=2 and heir was at index 0), then
    // continues through t_one → leaf_one. leaf_two is unreachable.
    expect(ids).toEqual(["u1", "a1", "t_two", "t_one", "leaf_one"]);
    expect(ids).not.toContain("leaf_two");
  });
});

describe("convertHistory — referential cache (P1)", () => {
  it("returns the same array reference for repeated calls with the same rows", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "hi" }),
      makeRow({ id: "a1", role: "assistant", parent_message_id: "u1", content: "yo" }),
    ];
    const a = convertHistory(rows);
    const b = convertHistory(rows);
    expect(b).toBe(a);
  });

  it("recomputes when given a different rows array", () => {
    const rows1: MessageRow[] = [makeRow({ id: "u1", role: "user", content: "hi" })];
    const rows2: MessageRow[] = [makeRow({ id: "u1", role: "user", content: "hi" })];
    expect(convertHistory(rows2)).not.toBe(convertHistory(rows1));
  });
});
