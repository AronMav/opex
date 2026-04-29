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
