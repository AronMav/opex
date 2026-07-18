/**
 * G4 / Task 6 Step 3: returning to the SAME session that is still actively
 * streaming (e.g. a deep-link or cross-agent picker re-resolving to the
 * session already open) must resume via the existing live stream / later
 * GET-replay — NOT force-settle the streaming message. `selectSession`
 * already had this early-return guard ("just switch to live view"); this
 * covers `selectSessionById`, whose unconditional `abortLocalOnly(agent)`
 * (default settleMessages:true) sweeps a live "streaming" message to
 * "complete" and resets connectionPhase/messageSource even when the target
 * session is IDENTICAL to the one already active. Gate the teardown on
 * "different session" only.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

const { mockInvalidate, mockAbortLocalOnly } = vi.hoisted(() => ({
  mockInvalidate: vi.fn(),
  mockAbortLocalOnly: vi.fn(),
}));

vi.mock("@/lib/query-client", () => ({
  queryClient: {
    invalidateQueries: mockInvalidate,
    setQueryData: vi.fn(),
    getQueryData: vi.fn(() => undefined),
  },
}));

vi.mock("@/stores/streaming-renderer", () => ({
  createStreamingRenderer: () => ({
    sendTurn: vi.fn(),
    connect: vi.fn(),
    resumeStream: vi.fn(),
    abortActiveStream: vi.fn(),
    abortLocalOnly: mockAbortLocalOnly,
    cleanupAgent: vi.fn(),
    getAbortCtrl: vi.fn(),
    setAbortCtrl: vi.fn(),
    getReconnectTimer: vi.fn(),
    setReconnectTimer: vi.fn(),
    onSessionId: vi.fn(),
  }),
}));

vi.mock("@/lib/api", () => ({
  apiGet: vi.fn().mockResolvedValue({}),
  apiPost: vi.fn().mockResolvedValue({}),
  apiPut: vi.fn().mockResolvedValue({}),
  apiPatch: vi.fn().mockResolvedValue({}),
  apiDelete: vi.fn().mockResolvedValue(undefined),
  getToken: () => "t",
  assertToken: () => "t",
}));

import { useChatStore } from "@/stores/chat-store";
import { emptyAgentState } from "@/stores/chat-types";

beforeEach(() => {
  mockInvalidate.mockClear();
  mockAbortLocalOnly.mockClear();
  useChatStore.setState({ currentAgent: "Agent1", agents: {}, sessionParticipants: {} });
});

describe("selectSessionById — return to the SAME active session (G4)", () => {
  it("does NOT abort/settle when re-selecting the session already streaming", () => {
    useChatStore.setState({
      currentAgent: "Agent1",
      agents: {
        Agent1: {
          ...emptyAgentState(),
          activeSessionId: "session-A",
          connectionPhase: "streaming",
          messageSource: {
            mode: "live",
            messages: [
              { id: "a1", role: "assistant", parts: [{ type: "text", text: "partial" }], status: "streaming" },
            ],
          },
        },
      },
    });

    useChatStore.getState().selectSessionById("Agent1", "session-A");

    expect(mockAbortLocalOnly).not.toHaveBeenCalled();
    // Phase and message untouched — the live stream keeps running in place.
    const st = useChatStore.getState().agents["Agent1"];
    expect(st.connectionPhase).toBe("streaming");
    const src = st.messageSource;
    const msgs = src.mode === "live" || src.mode === "finishing" ? src.messages : [];
    expect(msgs.find((m) => m.id === "a1")?.status).toBe("streaming");
  });

  it("still tears down (abortLocalOnly + settle) when switching to a DIFFERENT session", () => {
    useChatStore.setState({
      currentAgent: "Agent1",
      agents: {
        Agent1: {
          ...emptyAgentState(),
          activeSessionId: "session-A",
          connectionPhase: "streaming",
        },
      },
    });

    useChatStore.getState().selectSessionById("Agent1", "session-B");

    expect(mockAbortLocalOnly).toHaveBeenCalledWith("Agent1");
    const st = useChatStore.getState().agents["Agent1"];
    expect(st.activeSessionId).toBe("session-B");
    expect(st.connectionPhase).toBe("idle");
  });

  it("still switches currentAgent + selects the session when re-selecting same session but IDLE (not active)", () => {
    // Guard is scoped to active phases only — an idle same-session re-select
    // (e.g. plain history reload) keeps its existing normalize-to-history behavior.
    useChatStore.setState({
      currentAgent: "Agent2",
      agents: {
        Agent1: {
          ...emptyAgentState(),
          activeSessionId: "session-A",
          connectionPhase: "idle",
        },
      },
    });

    useChatStore.getState().selectSessionById("Agent1", "session-A");

    expect(useChatStore.getState().currentAgent).toBe("Agent1");
    expect(mockAbortLocalOnly).toHaveBeenCalledWith("Agent1");
  });
});
