import { describe, it, expect } from "vitest";
import { getLiveMessages } from "@/stores/chat-types";
import { selectIsLive, selectIsReplayingHistory, selectLiveHasContent } from "@/stores/chat-selectors";
import type { MessageSource, ChatState } from "@/stores/chat-types";

// ── getLiveMessages ────────────────────────────────────────────────────────────

describe("getLiveMessages", () => {
  it("returns messages from live mode", () => {
    const src: MessageSource = { mode: "live", messages: [{ id: "1", role: "user", parts: [] }] };
    expect(getLiveMessages(src)).toHaveLength(1);
  });

  it("returns messages from finishing mode", () => {
    const src: MessageSource = {
      mode: "finishing",
      sessionId: "s1",
      messages: [{ id: "2", role: "assistant", parts: [] }],
    };
    expect(getLiveMessages(src)).toHaveLength(1);
  });

  it("returns empty array for history mode", () => {
    const src: MessageSource = { mode: "history", sessionId: "s1" };
    expect(getLiveMessages(src)).toHaveLength(0);
  });

  it("returns empty array for new-chat mode", () => {
    const src: MessageSource = { mode: "new-chat" };
    expect(getLiveMessages(src)).toHaveLength(0);
  });
});

// ── Selectors: finishing mode is NOT live, NOT history ─────────────────────────

function fakeState(mode: string, extra: Record<string, unknown> = {}): ChatState {
  return {
    agents: {
      Arty: {
        messageSource: { mode, ...extra },
        selectedBranches: {},
        activeSessionId: null,
      },
    },
    currentAgent: "Arty",
  } as unknown as ChatState;
}

describe("selectors with finishing mode", () => {
  it("selectIsLive returns false for finishing", () => {
    const s = fakeState("finishing", { sessionId: "s1", messages: [] });
    expect(selectIsLive(s, "Arty")).toBe(false);
  });

  it("selectIsReplayingHistory returns false for finishing", () => {
    const s = fakeState("finishing", { sessionId: "s1", messages: [] });
    expect(selectIsReplayingHistory(s, "Arty")).toBe(false);
  });

  it("selectLiveHasContent returns false for finishing (frozen, not streaming)", () => {
    const s = fakeState("finishing", {
      sessionId: "s1",
      messages: [{ id: "x", role: "assistant", parts: [{ type: "text", text: "hi" }] }],
    });
    expect(selectLiveHasContent(s, "Arty")).toBe(false);
  });

  it("selectIsLive returns true for live mode", () => {
    const s = fakeState("live", { messages: [{ id: "y", role: "user", parts: [] }] });
    expect(selectIsLive(s, "Arty")).toBe(true);
  });
});

// ── finishing mode contract ────────────────────────────────────────────────────

describe("finishing mode contract", () => {
  it("holds messages while history is loading", () => {
    const liveMsg = {
      id: "live-1",
      role: "assistant" as const,
      parts: [{ type: "text" as const, text: "hello" }],
    };
    const src: MessageSource = { mode: "finishing", sessionId: "s1", messages: [liveMsg] };
    expect(getLiveMessages(src)).toContainEqual(liveMsg);
  });

  it("history mode has no live messages (transition complete)", () => {
    const src: MessageSource = { mode: "history", sessionId: "s1" };
    expect(getLiveMessages(src)).toHaveLength(0);
  });
});
