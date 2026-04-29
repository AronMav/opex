import { describe, it, expect } from "vitest";
import { convertHistory } from "@/stores/chat-store";
import type { MessageRow } from "@/types/api";

// ── Helper ──────────────────────────────────────────────────────────────────

function makeRow(overrides: Partial<MessageRow> & { id: string }): MessageRow {
  return {
    role: "assistant",
    content: "test content",
    tool_calls: null,
    tool_call_id: null,
    created_at: new Date().toISOString(),
    agent_id: null,
    status: "complete",
    feedback: 0,
    edited_at: null,
    thinking_blocks: null,
    parent_message_id: null,
    branch_from_message_id: null,
    abort_reason: null,
    ...overrides,
  };
}

// ── Tests ───────────────────────────────────────────────────────────────────

describe("convertHistory — message identity", () => {
  it("separates messages from different agents", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "a1", agent_id: "Agent1", content: "Hello from Agent1" }),
      makeRow({ id: "a2", agent_id: "Helper", content: "Hello from Helper" }),
    ];

    const messages = convertHistory(rows);

    expect(messages).toHaveLength(2);
    expect(messages[0].agentId).toBe("Agent1");
    expect(messages[1].agentId).toBe("Helper");
  });

  it("separates consecutive assistant messages from same agent (D-01 no merge)", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "a1", agent_id: "Agent1", content: "First message" }),
      makeRow({ id: "a2", agent_id: "Agent1", content: "Second message" }),
    ];

    const messages = convertHistory(rows);

    // D-01: Each assistant DB row = separate ChatMessage (no merge)
    expect(messages).toHaveLength(2);
    expect(messages[0].id).toBe("a1");
    expect(messages[1].id).toBe("a2");
  });

  it("tool results attach to correct parent assistant (Virtual Merging)", () => {
    const rows: MessageRow[] = [
      makeRow({
        id: "a1",
        agent_id: "Agent1",
        content: "",
        tool_calls: [{ id: "tc1", name: "search", arguments: "{}" }],
      }),
      makeRow({
        id: "t1",
        role: "tool",
        content: "search result",
        tool_call_id: "tc1",
        agent_id: null,
      }),
      makeRow({
        id: "a2",
        agent_id: "Agent1",
        content: "Based on the search...",
      }),
    ];

    const messages = convertHistory(rows);

    // D-01: No merge — 2 assistant messages + tool result attached to first
    expect(messages).toHaveLength(2);
    expect(messages[0].agentId).toBe("Agent1");
    // First assistant has tool parts (tool result attaches to it)
    const toolParts = messages[0].parts.filter((p) => p.type === "tool");
    expect(toolParts).toHaveLength(1);
    // Second assistant has the text
    const textParts = messages[1].parts.filter((p) => p.type === "text");
    expect(textParts).toHaveLength(1);
    expect(textParts.some(p => "text" in p && p.text.includes("Based on the search"))).toBe(true);
  });

  it("empty-content assistant rows are filtered out", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "a1", content: "", tool_calls: null }),
    ];

    const messages = convertHistory(rows);

    expect(messages).toHaveLength(0);
  });

  it("tool-only rows with whitespace text are NOT filtered out", () => {
    // Review concern: empty-content filter must not drop tool-only rows.
    // An assistant row with whitespace-only content BUT with tool_calls
    // should be retained because it has tool parts after tool result attachment.
    const rows: MessageRow[] = [
      makeRow({
        id: "a1",
        agent_id: "Agent1",
        content: " ",
        tool_calls: [{ id: "tc1", name: "search", arguments: "{}" }],
      }),
      makeRow({
        id: "t1",
        role: "tool",
        content: "search result",
        tool_call_id: "tc1",
        agent_id: null,
      }),
    ];

    const messages = convertHistory(rows);

    // The message should be present because it has tool parts
    expect(messages).toHaveLength(1);
    expect(messages[0].agentId).toBe("Agent1");
    const toolParts = messages[0].parts.filter((p) => p.type === "tool");
    expect(toolParts).toHaveLength(1);
  });

  it("legacy messages without agent_id get agentId=undefined", () => {
    // D-10 fallback to currentAgent happens at render time in AssistantMessage,
    // not in convertHistory(). currentAgent = useChatStore(s => s.currentAgent)
    // = the agent selected in the header dropdown.
    const rows: MessageRow[] = [
      makeRow({ id: "a1", agent_id: null, content: "Hello" }),
    ];

    const messages = convertHistory(rows);

    expect(messages).toHaveLength(1);
    expect(messages[0].agentId).toBeUndefined();
  });

  it("parity: history produces same structure as streaming", () => {
    // Full multi-agent sequence — different agents don't merge, same agents do
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "Hello", agent_id: null }),
      makeRow({ id: "a1", agent_id: "Agent1", content: "Hi from Agent1" }),
      makeRow({ id: "a2", agent_id: "Helper", content: "Hi from Helper" }),
      makeRow({ id: "u2", role: "user", content: "Thanks", agent_id: null }),
      makeRow({ id: "a3", agent_id: "Agent1", content: "You're welcome" }),
    ];

    const messages = convertHistory(rows);

    // Different agents and user messages break the merge, so no merging here
    expect(messages).toHaveLength(5);
    expect(messages.map((m) => m.role)).toEqual([
      "user",
      "assistant",
      "assistant",
      "user",
      "assistant",
    ]);
    expect(messages.map((m) => m.agentId)).toEqual([
      undefined,
      "Agent1",
      "Helper",
      undefined,
      "Agent1",
    ]);
  });

  it("propagates abort_reason and aborted status from row", () => {
    // When backend persists a partial assistant row (status='aborted',
    // abort_reason=<stable id>), convertHistory must surface both on the
    // ChatMessage so <AssistantMessage>'s footer lights up on history load.
    const rows: MessageRow[] = [
      makeRow({
        id: "m1",
        agent_id: "Agent1",
        content: "partial",
        status: "aborted",
        abort_reason: "inactivity",
      }),
    ];

    const messages = convertHistory(rows);

    expect(messages).toHaveLength(1);
    expect(messages[0].status).toBe("aborted");
    expect(messages[0].abortReason).toBe("inactivity");
  });

  it("non-aborted rows do not set status or abortReason", () => {
    // Sanity: a plain complete row must not carry the abort markers, else
    // the footer would render unconditionally on every history load.
    const rows: MessageRow[] = [
      makeRow({ id: "a1", agent_id: "Agent1", content: "hello", status: "complete" }),
    ];

    const messages = convertHistory(rows);

    expect(messages).toHaveLength(1);
    expect(messages[0].status).toBeUndefined();
    expect(messages[0].abortReason).toBeUndefined();
  });

  it("aborted row with null abort_reason still marks status", () => {
    // Back-compat: historical rows inserted before abort_reason was plumbed
    // may have status='aborted' with a NULL reason. We still mark the
    // message as aborted so the footer can render a generic label.
    const rows: MessageRow[] = [
      makeRow({
        id: "m1",
        agent_id: "Agent1",
        content: "partial",
        status: "aborted",
        abort_reason: null,
      }),
    ];

    const messages = convertHistory(rows);

    expect(messages).toHaveLength(1);
    expect(messages[0].status).toBe("aborted");
    expect(messages[0].abortReason).toBeNull();
  });

  it("parity with tool grouping structure", () => {
    const rows: MessageRow[] = [
      makeRow({
        id: "a1",
        agent_id: "Agent1",
        content: "",
        tool_calls: [{ id: "tc1", name: "search", arguments: "{}" }],
      }),
      makeRow({
        id: "t1",
        role: "tool",
        content: "result data",
        tool_call_id: "tc1",
        agent_id: null,
      }),
      makeRow({
        id: "a2",
        agent_id: "Helper",
        content: "Here is what I found",
      }),
    ];

    const messages = convertHistory(rows);

    expect(messages).toHaveLength(2);
    // First message: Agent1 with tool parts
    expect(messages[0].agentId).toBe("Agent1");
    expect(messages[0].parts.some((p) => p.type === "tool")).toBe(true);
    // Second message: Helper with text parts
    expect(messages[1].agentId).toBe("Helper");
    expect(messages[1].parts.some((p) => p.type === "text")).toBe(true);
    expect(messages[1].parts.some((p) => p.type === "tool")).toBe(false);
  });
});
