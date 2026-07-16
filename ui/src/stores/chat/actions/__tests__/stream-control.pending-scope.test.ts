/**
 * Fix H: pendingMessage is session/turn-scoped.
 *  - queueMessage stamps the message with the session id + agent it was queued
 *    for (so the ChatThread drain can verify the context still matches).
 *  - setCurrentAgent clears the DEPARTING agent's pending (the LOST case): after
 *    an agent switch the idle-transition edge the drain watches is never
 *    observed, so the message would otherwise be silently stranded forever.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/lib/query-client", () => ({
  queryClient: {
    invalidateQueries: vi.fn(),
    setQueryData: vi.fn(),
    getQueryData: vi.fn(() => undefined),
    getQueriesData: vi.fn(() => []),
  },
}));

vi.mock("@/stores/streaming-renderer", () => ({
  createStreamingRenderer: () => ({
    sendTurn: vi.fn(),
    connect: vi.fn(),
    resumeStream: vi.fn(),
    abortActiveStream: vi.fn(),
    abortLocalOnly: vi.fn(),
    cleanupAgent: vi.fn(),
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

const A = "AgentA";
const B = "AgentB";

beforeEach(() => {
  useChatStore.setState({
    currentAgent: A,
    agents: { [A]: { ...emptyAgentState(), activeSessionId: "S1" } },
    sessionParticipants: {},
  });
});

describe("Fix H — queueMessage stamps session + agent", () => {
  it("stamps the current agent's activeSessionId and agent name", () => {
    useChatStore.getState().queueMessage("hi");
    const pending = useChatStore.getState().agents[A]?.pendingMessage;
    expect(pending?.content).toBe("hi");
    expect(pending?.sessionId).toBe("S1");
    expect(pending?.agent).toBe(A);
  });

  it("stamps sessionId=null when there is no active session", () => {
    useChatStore.setState({
      currentAgent: A,
      agents: { [A]: { ...emptyAgentState(), activeSessionId: null } },
    });
    useChatStore.getState().queueMessage("hi", undefined, { voice: true });
    const pending = useChatStore.getState().agents[A]?.pendingMessage;
    expect(pending?.sessionId).toBeNull();
    expect(pending?.agent).toBe(A);
    expect(pending?.voice).toBe(true);
  });
});

describe("Fix H — setCurrentAgent clears the departing agent's pending (LOST case)", () => {
  it("drops a queued message when switching to a different agent (no silent stranding)", () => {
    // Queue on A, then switch to B.
    useChatStore.getState().queueMessage("для A");
    expect(useChatStore.getState().agents[A]?.pendingMessage).not.toBeNull();

    useChatStore.getState().setCurrentAgent(B);

    // A's queue is cleared (surfaced via toast), not silently stuck.
    expect(useChatStore.getState().agents[A]?.pendingMessage).toBeNull();
    expect(useChatStore.getState().currentAgent).toBe(B);
  });
});
