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
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";
import type { MessageRow } from "@/types/api";

// ── Boundary-render dedup helpers (id-based seam dedup) ─────────────────────
//
// Root cause under test: `sendMessage` pre-allocates the turn's user message
// id and the server persists + echoes it back as `boundaryMessageId` on
// `sync_begin`. `historyUpToIncluding` is INCLUSIVE of that id, and the live
// overlay's optimistic user echo carries the SAME id — so once history is
// refetched mid-turn, the user message exists in BOTH the history slice and
// the live overlay, and renders twice unless deduped at the seam.

function seedHistory(sessionId: string, agent: string, rows: MessageRow[]): void {
  queryClient.setQueryData([...qk.sessionMessages(sessionId), agent], { messages: rows });
}

function makeUserRow(id: string, content: string, createdAt: string): MessageRow {
  return {
    id,
    role: "user",
    content,
    tool_calls: null,
    tool_call_id: null,
    created_at: createdAt,
    agent_id: null,
    feedback: null,
    edited_at: null,
    status: "done",
    thinking_blocks: null,
    parent_message_id: null,
    branch_from_message_id: null,
    abort_reason: null,
    is_mirror: false,
  };
}

function makeAssistantRow(id: string, content: string, createdAt: string, agent: string): MessageRow {
  return {
    id,
    role: "assistant",
    content,
    tool_calls: null,
    tool_call_id: null,
    created_at: createdAt,
    agent_id: agent,
    feedback: null,
    edited_at: null,
    status: "done",
    thinking_blocks: null,
    parent_message_id: null,
    branch_from_message_id: null,
    abort_reason: null,
    is_mirror: false,
  };
}

function countById(messages: ReturnType<typeof selectRenderMessages>, id: string): number {
  return messages.filter((m) => m.id === id).length;
}

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
    // tested via the chat-history overlay-dedup unit tests; here we only
    // guard the mode-switch dispatch.

    // ── Boundary dup bug (T8 seam) ─────────────────────────────────────────
    it("case 1: fresh send, history already contains the boundary user message — U1 renders ONCE (bug repro)", () => {
      const sessionId = "sess-dup-1";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeAssistantRow("a0", "reply one", "2026-07-15T00:00:01Z", agent),
        makeUserRow("b", "second message", "2026-07-15T00:00:02Z"),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
        boundaryMessageId: "b",
        messageSource: {
          mode: "finishing",
          sessionId,
          messages: [
            { id: "b", role: "user", parts: [{ type: "text", text: "second message" }] },
            { id: "a1", role: "assistant", parts: [{ type: "text", text: "reply two" }] },
          ],
        },
      });

      const rendered = selectRenderMessages(state, agent);

      expect(countById(rendered, "b")).toBe(1);
      expect(rendered.map((m) => m.id)).toEqual(["u0", "a0", "b", "a1"]);
    });

    it("case 1b: same repro in live mode (not just finishing)", () => {
      const sessionId = "sess-dup-1b";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeAssistantRow("a0", "reply one", "2026-07-15T00:00:01Z", agent),
        makeUserRow("b", "second message", "2026-07-15T00:00:02Z"),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
        boundaryMessageId: "b",
        messageSource: {
          mode: "live",
          messages: [
            { id: "b", role: "user", parts: [{ type: "text", text: "second message" }] },
            { id: "a1", role: "assistant", parts: [{ type: "text", text: "reply two" }] },
          ],
        },
      });

      const rendered = selectRenderMessages(state, agent);

      expect(countById(rendered, "b")).toBe(1);
      expect(rendered.map((m) => m.id)).toEqual(["u0", "a0", "b", "a1"]);
    });

    it("case 2: fresh send, history NOT yet refetched — boundary absent from history, live echo still renders", () => {
      const sessionId = "sess-dup-2";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeAssistantRow("a0", "reply one", "2026-07-15T00:00:01Z", agent),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
        boundaryMessageId: "b",
        messageSource: {
          mode: "live",
          messages: [
            { id: "b", role: "user", parts: [{ type: "text", text: "second message" }] },
            { id: "a1", role: "assistant", parts: [{ type: "text", text: "reply two" }] },
          ],
        },
      });

      const rendered = selectRenderMessages(state, agent);

      expect(rendered.map((m) => m.id)).toEqual(["u0", "a0", "b", "a1"]);
    });

    it("case 3: resume — live overlay has no optimistic user echo, only the assistant continuation", () => {
      const sessionId = "sess-dup-3";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeAssistantRow("a0", "reply one", "2026-07-15T00:00:01Z", agent),
        makeUserRow("b", "second message", "2026-07-15T00:00:02Z"),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
        boundaryMessageId: "b",
        messageSource: {
          mode: "live",
          messages: [
            { id: "a1", role: "assistant", parts: [{ type: "text", text: "reply two" }] },
          ],
        },
      });

      const rendered = selectRenderMessages(state, agent);

      expect(rendered.map((m) => m.id)).toEqual(["u0", "a0", "b", "a1"]);
    });

    it("case 4: null boundary — dedup still drops any live message whose id is already in full history", () => {
      const sessionId = "sess-dup-4";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeAssistantRow("a0", "reply one", "2026-07-15T00:00:01Z", agent),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
        boundaryMessageId: null,
        messageSource: {
          mode: "live",
          messages: [
            { id: "a0", role: "assistant", parts: [{ type: "text", text: "reply one" }] },
            { id: "a1", role: "assistant", parts: [{ type: "text", text: "reply two" }] },
          ],
        },
      });

      const rendered = selectRenderMessages(state, agent);

      expect(countById(rendered, "a0")).toBe(1);
      expect(rendered.map((m) => m.id)).toEqual(["u0", "a0", "a1"]);
    });
  });
});
