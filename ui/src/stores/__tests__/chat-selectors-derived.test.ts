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

// ── Render-merge cases (id-keyed merge over FULL history) ──────────────────
//
// Root cause the merge fixes: `sendMessage` pre-allocates the turn's user
// message id; the server persists the row under that same id. The optimistic
// user echo in the live overlay carries the SAME id. Once history refetches
// mid-turn the user row exists in BOTH history and the live overlay. The old
// positional model (inclusive boundary slice + concat) rendered it twice; the
// id-keyed merge renders each id once (live wins for shared ids).

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

    function renderedText(msg: ReturnType<typeof selectRenderMessages>[number]): string {
      return msg.parts.flatMap((p) => (p.type === "text" ? [p.text] : [])).join("");
    }

    // ── Render-merge cases ─────────────────────────────────────────────────
    it("case 1: fresh send, history refetched mid-turn — user id renders ONCE", () => {
      // history [U0, A0, U1(id=b)]  +  live [U1(id=b echo), A1] → [U0,A0,U1,A1]
      const sessionId = "sess-merge-1";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeAssistantRow("a0", "reply one", "2026-07-15T00:00:01Z", agent),
        makeUserRow("b", "second message", "2026-07-15T00:00:02Z"),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
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
      const sessionId = "sess-merge-1b";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeAssistantRow("a0", "reply one", "2026-07-15T00:00:01Z", agent),
        makeUserRow("b", "second message", "2026-07-15T00:00:02Z"),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
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

    it("case 2: fresh send, history NOT yet refetched — live echo appends", () => {
      // history [U0, A0]  +  live [U1(id=b), A1] → [U0,A0,U1,A1]
      const sessionId = "sess-merge-2";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeAssistantRow("a0", "reply one", "2026-07-15T00:00:01Z", agent),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
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

    it("case 3: resume, partial assistant persisted under shared id — LIVE WINS", () => {
      // history [..., U1, A1_partial(id=X)]  +  live [A1_live(id=X, fuller)]
      // shared id X → live wins → shows the fuller live text, NOT the stale partial.
      const sessionId = "sess-merge-3";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeUserRow("u1", "question", "2026-07-15T00:00:02Z"),
        makeAssistantRow("X", "partial", "2026-07-15T00:00:03Z", agent),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
        messageSource: {
          mode: "live",
          messages: [
            { id: "X", role: "assistant", parts: [{ type: "text", text: "partial and then much more" }] },
          ],
        },
      });

      const rendered = selectRenderMessages(state, agent);

      expect(countById(rendered, "X")).toBe(1);
      expect(rendered.map((m) => m.id)).toEqual(["u0", "u1", "X"]);
      // live-wins: the rendered X is the fuller live copy, not the stale partial.
      const x = rendered.find((m) => m.id === "X")!;
      expect(renderedText(x)).toBe("partial and then much more");
    });

    it("case 4: resume, assistant not yet persisted — live assistant appends", () => {
      // history [..., U1]  +  live [A1(id=X)] → [..., U1, X]
      const sessionId = "sess-merge-4";
      seedHistory(sessionId, agent, [
        makeUserRow("u0", "first", "2026-07-15T00:00:00Z"),
        makeUserRow("u1", "question", "2026-07-15T00:00:02Z"),
      ]);
      const state = makeState(agent, {
        activeSessionId: sessionId,
        messageSource: {
          mode: "live",
          messages: [
            { id: "X", role: "assistant", parts: [{ type: "text", text: "streaming reply" }] },
          ],
        },
      });

      const rendered = selectRenderMessages(state, agent);

      expect(rendered.map((m) => m.id)).toEqual(["u0", "u1", "X"]);
    });
  });
});
