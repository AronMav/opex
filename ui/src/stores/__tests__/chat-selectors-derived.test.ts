import { describe, it, expect } from "vitest";
import {
  selectRenderMessages,
  selectIsEmpty,
  selectIsReplayingHistory,
  selectIsLive,
  selectLiveHasContent,
} from "../chat-selectors";
import type { ChatState } from "../chat-types";
import { emptyAgentState } from "../chat-types";

// Minimal state factory — uses emptyAgentState() so the shape stays in sync
// with AgentState whenever new required fields are added.
function makeState(agent: string, overrides: Partial<ReturnType<typeof emptyAgentState>> = {}): ChatState {
  return {
    currentAgent: agent,
    agents: {
      [agent]: { ...emptyAgentState(), ...overrides },
    },
    sessionParticipants: {},
  } as unknown as ChatState;
}

describe("chat-selectors (derived)", () => {
  const agent = "Arty";

  describe("selectIsEmpty", () => {
    it("returns true for new-chat mode", () => {
      expect(selectIsEmpty(makeState(agent), agent)).toBe(true);
    });
    it("returns false for live mode", () => {
      expect(selectIsEmpty(makeState(agent, { messageSource: { mode: "live", messages: [] } }), agent)).toBe(false);
    });
    it("returns false for history mode", () => {
      expect(selectIsEmpty(makeState(agent, { messageSource: { mode: "history", sessionId: "x" } }), agent)).toBe(false);
    });
  });

  describe("selectIsReplayingHistory", () => {
    it("returns true for history mode", () => {
      expect(selectIsReplayingHistory(makeState(agent, { messageSource: { mode: "history", sessionId: "x" } }), agent)).toBe(true);
    });
    it("returns false otherwise", () => {
      expect(selectIsReplayingHistory(makeState(agent), agent)).toBe(false);
    });
  });

  describe("selectIsLive", () => {
    it("returns true for live mode regardless of messages length", () => {
      expect(selectIsLive(makeState(agent, { messageSource: { mode: "live", messages: [] } }), agent)).toBe(true);
    });
    it("returns false for history mode", () => {
      expect(selectIsLive(makeState(agent, { messageSource: { mode: "history", sessionId: "x" } }), agent)).toBe(false);
    });
  });

  describe("selectLiveHasContent", () => {
    it("returns true for live mode with ≥1 message", () => {
      const msg = { id: "m1", role: "assistant" as const, parts: [], createdAt: new Date().toISOString() };
      expect(selectLiveHasContent(makeState(agent, { messageSource: { mode: "live", messages: [msg] } }), agent)).toBe(true);
    });
    it("returns false for live mode with 0 messages", () => {
      expect(selectLiveHasContent(makeState(agent, { messageSource: { mode: "live", messages: [] } }), agent)).toBe(false);
    });
    it("returns false for history mode", () => {
      expect(selectLiveHasContent(makeState(agent, { messageSource: { mode: "history", sessionId: "x" } }), agent)).toBe(false);
    });
  });

  describe("selectRenderMessages", () => {
    it("returns [] for new-chat mode", () => {
      expect(selectRenderMessages(makeState(agent), agent)).toEqual([]);
    });
    // history mode / live mode / live overlay over history exhaustively
    // tested via existing chat-overlay-dedup.test.ts; here we only
    // guard the mode-switch dispatch.
  });
});
