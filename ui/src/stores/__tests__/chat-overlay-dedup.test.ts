import { describe, it, expect } from "vitest";
import { mergeLiveOverlay } from "@/stores/chat-overlay-dedup";
import type { ChatMessage } from "@/stores/chat-types";

// ── Helpers ─────────────────────────────────────────────────────────────────

function userMsg(
  id: string,
  text: string,
  status?: "sending" | "confirmed" | "failed",
): ChatMessage {
  return {
    id,
    role: "user",
    parts: [{ type: "text", text }],
    createdAt: new Date().toISOString(),
    status,
  };
}

function assistantMsg(id: string, text: string): ChatMessage {
  return {
    id,
    role: "assistant",
    parts: [{ type: "text", text }],
    createdAt: new Date().toISOString(),
  };
}

// ── Regression: 2026-04-17 "sent message disappears" ───────────────────────

describe("mergeLiveOverlay — user bubble visibility", () => {
  it("shows a SENDING user bubble when history is empty (fresh send)", () => {
    const history: ChatMessage[] = [];
    const live: ChatMessage[] = [userMsg("u1", "Hello agent", "sending")];
    const out = mergeLiveOverlay(history, live);
    expect(out).toHaveLength(1);
    expect(out[0].role).toBe("user");
    expect(out[0].id).toBe("u1");
  });

  it("STILL shows a CONFIRMED user bubble until history mirrors it (regression)", () => {
    // Before the fix, status === "confirmed" caused `continue` that dropped
    // the optimistic bubble, leaving chat empty while the agent worked.
    const history: ChatMessage[] = [];
    const live: ChatMessage[] = [userMsg("u1", "Hello agent", "confirmed")];
    const out = mergeLiveOverlay(history, live);
    expect(out).toHaveLength(1);
    expect(out[0].role).toBe("user");
    expect(out[0].status).toBe("confirmed");
  });

  it("STILL shows a FAILED user bubble until rollback UI replaces it", () => {
    const history: ChatMessage[] = [];
    const live: ChatMessage[] = [userMsg("u1", "bad message", "failed")];
    const out = mergeLiveOverlay(history, live);
    expect(out).toHaveLength(1);
    expect(out[0].status).toBe("failed");
  });

  it("DEDUPS when history already contains the same user text", () => {
    const history: ChatMessage[] = [
      userMsg("db-1", "Hello agent"),
      assistantMsg("db-2", "Hi!"),
    ];
    const live: ChatMessage[] = [userMsg("u1", "Hello agent", "confirmed")];
    const out = mergeLiveOverlay(history, live);
    // History has 2, live 1 — dedup removes live copy → still 2.
    expect(out).toHaveLength(2);
    expect(out[0].id).toBe("db-1"); // history user survives
    // No "u1" in the output.
    expect(out.every((m) => m.id !== "u1")).toBe(true);
  });

  it("shows BOTH history and live when texts differ (second send)", () => {
    const history: ChatMessage[] = [userMsg("db-1", "First")];
    const live: ChatMessage[] = [userMsg("u1", "Second", "sending")];
    const out = mergeLiveOverlay(history, live);
    expect(out).toHaveLength(2);
    expect(out[0].id).toBe("db-1");
    expect(out[1].id).toBe("u1");
  });
});

describe("mergeLiveOverlay — assistant dedup", () => {
  it("drops empty assistant placeholders", () => {
    const history: ChatMessage[] = [];
    const live: ChatMessage[] = [
      {
        id: "a1",
        role: "assistant",
        parts: [],
        createdAt: new Date().toISOString(),
      },
    ];
    expect(mergeLiveOverlay(history, live)).toEqual([]);
  });

  it("does NOT dedupe live assistant text by content fingerprint (false positives ate new-message starts)", () => {
    // Two assistant messages with identical text but different ids — e.g. the
    // model repeats a planning sentence at the start of a new turn. Live copy
    // must remain visible; only message-level ID dedup handles the genuine
    // history-catches-up case (covered by the next test).
    const history: ChatMessage[] = [assistantMsg("db-a", "Hello world")];
    const live: ChatMessage[] = [assistantMsg("live-a", "Hello world")];
    const out = mergeLiveOverlay(history, live);
    expect(out).toHaveLength(2);
    expect(out[0].id).toBe("db-a");
    expect(out[1].id).toBe("live-a");
  });

  it("filters live assistant whose id matches a history row (history caught up)", () => {
    // Server-side messageId is reused between SSE start event and the persisted
    // DB row, so when history refetches the same id appears in both → message-
    // level ID dedup hides the live duplicate cleanly.
    const history: ChatMessage[] = [assistantMsg("msg_42", "Hello world")];
    const live: ChatMessage[] = [assistantMsg("msg_42", "Hello world")];
    const out = mergeLiveOverlay(history, live);
    expect(out).toHaveLength(1);
    expect(out[0].id).toBe("msg_42");
  });

  it("strips tool parts already in history by toolCallId", () => {
    const historyWithTool: ChatMessage = {
      id: "db-a",
      role: "assistant",
      parts: [{ type: "tool", toolCallId: "tc1", toolName: "search", state: "output-available", input: {}, output: "" }],
      createdAt: new Date().toISOString(),
    };
    const liveWithSameTool: ChatMessage = {
      id: "live-a",
      role: "assistant",
      parts: [{ type: "tool", toolCallId: "tc1", toolName: "search", state: "output-available", input: {}, output: "" }],
      createdAt: new Date().toISOString(),
    };
    const out = mergeLiveOverlay([historyWithTool], [liveWithSameTool]);
    expect(out).toHaveLength(1);
    expect(out[0].id).toBe("db-a");
  });
});

// ── Continuation merge (multi-turn tool-loop) ────────────────────────────────

function assistantMsgWithAgent(id: string, text: string, agentId: string): ChatMessage {
  return {
    id,
    role: "assistant",
    parts: [{ type: "text", text }],
    createdAt: new Date().toISOString(),
    agentId,
  };
}

describe("mergeLiveOverlay — continuation merge (multi-turn tool loop)", () => {
  it("merges live continuation into last history assistant when same agent, no new user message", () => {
    // Scenario: iteration 1 done (in history), iteration 2 streaming (in live)
    const history: ChatMessage[] = [
      userMsg("u1", "Discuss in 3 cycles"),
      assistantMsgWithAgent("a1", "Gathering data...", "Arty"),
    ];
    // Live has confirmed user (already in history) + current iteration assistant
    const live: ChatMessage[] = [
      userMsg("u1", "Discuss in 3 cycles"),     // confirmed, same text as history
      assistantMsgWithAgent("live-a2", "Launching cycle 1...", "Arty"),
    ];

    const out = mergeLiveOverlay(history, live);

    // Should be ONE merged bubble, not two
    const assistants = out.filter(m => m.role === "assistant");
    expect(assistants).toHaveLength(1);

    // The merged assistant should have BOTH text parts
    const textParts = assistants[0].parts.filter(p => p.type === "text");
    expect(textParts.map((p: any) => p.text)).toContain("Gathering data...");
    expect(textParts.map((p: any) => p.text)).toContain("Launching cycle 1...");
  });

  it("deduplicates repeated preamble text in live continuation (same text in history and live)", () => {
    // Arty repeats "Gathering data..." at the start of EVERY iteration.
    // Live buffer has: [text1(from_it1), text1(from_it2), new_tool]
    // After tool dedup removes old tools, only text1(dup)+text1(dup)+new_tool remain.
    // Text dedup should remove the duplicates.
    const history: ChatMessage[] = [
      userMsg("u1", "Discuss world"),
      {
        id: "a1",
        role: "assistant",
        agentId: "Arty",
        parts: [
          { type: "text", text: "Gathering data..." },
          { type: "tool", toolCallId: "tc1", toolName: "search", state: "output-available", input: {}, output: "news" },
        ],
        createdAt: new Date().toISOString(),
      } as ChatMessage,
    ];

    const live: ChatMessage[] = [
      userMsg("u1", "Discuss world"),
      {
        id: "live-a2",
        role: "assistant",
        agentId: "Arty",
        parts: [
          { type: "tool", toolCallId: "tc1", toolName: "search", state: "output-available", input: {}, output: "news" }, // already in history → filtered
          { type: "text", text: "Gathering data..." },  // duplicate → filtered
          { type: "tool", toolCallId: "tc2", toolName: "agent", state: "output-available", input: {}, output: "result" }, // NEW
          { type: "text", text: "Now analyzing..." }, // new text
        ],
        createdAt: new Date().toISOString(),
      } as ChatMessage,
    ];

    const out = mergeLiveOverlay(history, live);
    const assistants = out.filter(m => m.role === "assistant");
    expect(assistants).toHaveLength(1); // merged into one bubble

    const parts = assistants[0].parts;
    // New tool should be present
    expect(parts.some((p: any) => p.type === "tool" && p.toolCallId === "tc2")).toBe(true);
    // New text should be present
    expect(parts.some((p: any) => p.type === "text" && p.text === "Now analyzing...")).toBe(true);
    // Duplicate text should NOT be duplicated in the merged output
    const gatheringCount = parts.filter((p: any) => p.type === "text" && p.text === "Gathering data...").length;
    expect(gatheringCount).toBe(1); // only from history, not duplicated
  });

  it("confirmed user message in history does NOT set liveHasNewUserMsg — continuation still merges", () => {
    // Regression: before the fix, ANY user message in live set liveHasNewUserMsg=true,
    // blocking continuation merge even when that user message was already confirmed in history.
    const history: ChatMessage[] = [
      userMsg("u1", "Discuss"),
      assistantMsgWithAgent("a1", "Searching...", "Arty"),
    ];
    const live: ChatMessage[] = [
      userMsg("u1", "Discuss"), // confirmed user — same text, already in history
      assistantMsgWithAgent("live-a2", "Analyzing...", "Arty"), // continuation
    ];

    const out = mergeLiveOverlay(history, live);
    const assistants = out.filter(m => m.role === "assistant");

    // Must merge into one bubble (not two separate ones)
    expect(assistants).toHaveLength(1);
    // Both texts must be present in the merged bubble
    const texts = assistants[0].parts.filter((p: any) => p.type === "text").map((p: any) => p.text);
    expect(texts).toContain("Searching...");
    expect(texts).toContain("Analyzing...");
  });

  it("genuinely new user message in live BLOCKS continuation merge (new turn)", () => {
    // A new user message that is NOT in history means a new turn is starting.
    // The live assistant should appear as a separate bubble after the user message.
    const history: ChatMessage[] = [
      userMsg("u1", "First question"),
      assistantMsgWithAgent("a1", "First answer", "Arty"),
    ];
    const live: ChatMessage[] = [
      userMsg("u2", "Second question"),  // new — NOT in history
      assistantMsgWithAgent("live-a2", "Thinking...", "Arty"),
    ];

    const out = mergeLiveOverlay(history, live);
    // History: user + assistant, Live overlay: new user + assistant
    // Total: 4 messages (no merge because new user message is present)
    expect(out).toHaveLength(4);
    expect(out[2].role).toBe("user");
    expect(out[3].role).toBe("assistant");
  });

  it("no continuation merge when live assistant has different agentId", () => {
    // Only merge when both history last assistant and live assistant share the same agentId
    const history: ChatMessage[] = [
      userMsg("u1", "Hello"),
      assistantMsgWithAgent("a1", "Hi from Arty", "Arty"),
    ];
    const live: ChatMessage[] = [
      userMsg("u1", "Hello"),
      assistantMsgWithAgent("live-a2", "Hi from Hyde", "Hyde"), // different agent
    ];

    const out = mergeLiveOverlay(history, live);
    // Different agentId → no merge → two assistant bubbles
    const assistants = out.filter(m => m.role === "assistant");
    expect(assistants).toHaveLength(2);
    expect(assistants[0].id).toBe("a1");
    expect(assistants[1].id).toBe("live-a2");
  });
});

describe("mergeLiveOverlay — edge cases", () => {
  it("returns history unchanged when live is empty", () => {
    const history: ChatMessage[] = [assistantMsg("a1", "hi")];
    expect(mergeLiveOverlay(history, [])).toBe(history);
  });

  it("returns history unchanged when live overlay has only id-duplicates", () => {
    // Live items must share IDs with history rows (which is what happens once
    // server-side messageId echoes back through history refetch) for the
    // overlay to collapse cleanly. Different-id live copies of identical text
    // are intentionally preserved — see "does NOT dedupe live assistant text"
    // above for the rationale.
    const history: ChatMessage[] = [
      userMsg("db-u", "Hello"),
      assistantMsg("db-a", "Hi there"),
    ];
    const live: ChatMessage[] = [
      userMsg("db-u", "Hello", "confirmed"),
      assistantMsg("db-a", "Hi there"),
    ];
    const out = mergeLiveOverlay(history, live);
    expect(out).toEqual(history);
  });
});
