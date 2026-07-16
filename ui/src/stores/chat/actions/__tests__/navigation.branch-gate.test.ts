/**
 * Fix L: branch switching must be gated while a turn is active for the session.
 * Switching an earlier branch mid-turn re-walks resolveActivePath to a
 * different trunk, so the live overlay (old branch's lineage) renders after a
 * different branch's history — two branches blended. switchBranch is a no-op
 * during an active phase and works normally when idle.
 */
import { describe, it, expect, vi } from "vitest";

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
import { selectCurrentPhaseIsActive } from "@/stores/chat-selectors";

const AGENT = "main";

function seed(phase: "streaming" | "submitted" | "idle") {
  useChatStore.setState({
    currentAgent: AGENT,
    agents: {
      [AGENT]: {
        ...emptyAgentState(),
        activeSessionId: "S1",
        connectionPhase: phase,
        messageSource: { mode: "history", sessionId: "S1" },
        selectedBranches: {},
      },
    },
  });
}

describe("Fix L — branch-nav gate", () => {
  it("switchBranch is a no-op while the turn is active (streaming)", () => {
    seed("streaming");
    useChatStore.getState().switchBranch("p1", "child-b");
    expect(useChatStore.getState().agents[AGENT]?.selectedBranches).toEqual({});
  });

  it("switchBranch is a no-op while submitted", () => {
    seed("submitted");
    useChatStore.getState().switchBranch("p1", "child-b");
    expect(useChatStore.getState().agents[AGENT]?.selectedBranches).toEqual({});
  });

  it("switchBranch works when idle", () => {
    seed("idle");
    useChatStore.getState().switchBranch("p1", "child-b");
    expect(useChatStore.getState().agents[AGENT]?.selectedBranches).toEqual({ p1: "child-b" });
  });

  it("selectCurrentPhaseIsActive reflects the current agent's phase", () => {
    seed("streaming");
    expect(selectCurrentPhaseIsActive(useChatStore.getState())).toBe(true);
    seed("idle");
    expect(selectCurrentPhaseIsActive(useChatStore.getState())).toBe(false);
  });
});
