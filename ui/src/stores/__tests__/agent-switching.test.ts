/**
 * Agent switching — TDD contract tests.
 *
 * These tests define the DESIRED behaviour after the bug fix:
 * "переключение агента не происходит сразу, а только со второго раза"
 *
 * Root cause: setCurrentAgent reset activeSessionId → null and messageSource →
 * "new-chat", so the first render showed a blank screen. The useEffect restore
 * ran only AFTER the browser painted, causing a visible flash that the user
 * perceived as "switch didn't happen".
 *
 * Fix contract (what these tests verify):
 *  1. setCurrentAgent pre-populates activeSessionId from localStorage so the
 *     first render immediately shows the agent's last session.
 *  2. forceNewSession is false when resuming a known session (true only for
 *     brand-new agents with no prior session).
 *  3. The localStorage session ID is NOT cleared on switch — it is preserved
 *     so subsequent switches also benefit from pre-population.
 *  4. page.tsx restore effect validates the pre-populated session against the
 *     live sessions list and falls through to sessions[0] when it is stale.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// ── Mocks (must be hoisted before imports) ───────────────────────────────────

const { mockInvalidate } = vi.hoisted(() => ({ mockInvalidate: vi.fn() }));

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
    abortLocalOnly: vi.fn(),
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

import { useChatStore, saveLastSession, getLastSessionId } from "@/stores/chat-store";
import { emptyAgentState } from "@/stores/chat-types";

// ── Shared setup ─────────────────────────────────────────────────────────────

function agentWithSession(sessionId: string) {
  return {
    ...emptyAgentState(),
    activeSessionId: sessionId,
    messageSource: { mode: "history" as const, sessionId },
  };
}

beforeEach(() => {
  mockInvalidate.mockClear();
  localStorage.clear();
  useChatStore.setState({
    currentAgent: "Agent1",
    agents: { Agent1: agentWithSession("sess-agent1") },
    sessionParticipants: {},
  });
});

afterEach(() => {
  localStorage.clear();
  useChatStore.setState({ agents: {}, currentAgent: "" });
});

// ── 1. Pre-population from localStorage ─────────────────────────────────────

describe("setCurrentAgent — pre-populates state from localStorage", () => {
  it("sets activeSessionId to the stored session when one exists", () => {
    saveLastSession("Agent2", "sess-agent2-stored");

    useChatStore.getState().setCurrentAgent("Agent2");

    expect(useChatStore.getState().agents["Agent2"]?.activeSessionId).toBe(
      "sess-agent2-stored"
    );
  });

  it("sets messageSource to history mode when a prior session exists", () => {
    saveLastSession("Agent2", "sess-agent2-stored");

    useChatStore.getState().setCurrentAgent("Agent2");

    expect(useChatStore.getState().agents["Agent2"]?.messageSource).toEqual({
      mode: "history",
      sessionId: "sess-agent2-stored",
    });
  });

  it("sets forceNewSession: false when resuming a known session", () => {
    saveLastSession("Agent2", "sess-agent2-stored");

    useChatStore.getState().setCurrentAgent("Agent2");

    expect(useChatStore.getState().agents["Agent2"]?.forceNewSession).toBe(false);
  });
});

// ── 2. New-chat state when no prior session ──────────────────────────────────

describe("setCurrentAgent — new-chat state when no prior session exists", () => {
  it("does NOT use another agent's legacy global sessionId as pre-population", () => {
    // Simulate legacy localStorage: global sessionId set (e.g. Arty's last session),
    // but no per-agent entry for Agent2. Without the fix, getLastSessionId("Agent2")
    // returned the global sessionId, causing the cross-agent resolver to switch
    // back to the agent that owned that session.
    const raw = { agent: "Agent1", sessions: { Agent1: "sess-agent1" }, sessionId: "sess-agent1" };
    localStorage.setItem("opex.chat.lastSession", JSON.stringify(raw));

    useChatStore.getState().setCurrentAgent("Agent2");

    // Must be null — never another agent's session
    expect(useChatStore.getState().agents["Agent2"]?.activeSessionId).toBeNull();
    expect(useChatStore.getState().agents["Agent2"]?.messageSource).toEqual({ mode: "new-chat" });
  });

  it("sets activeSessionId to null", () => {
    // No localStorage entry for Agent2
    useChatStore.getState().setCurrentAgent("Agent2");

    expect(useChatStore.getState().agents["Agent2"]?.activeSessionId).toBeNull();
  });

  it("sets messageSource to new-chat", () => {
    useChatStore.getState().setCurrentAgent("Agent2");

    expect(useChatStore.getState().agents["Agent2"]?.messageSource).toEqual({
      mode: "new-chat",
    });
  });

  it("sets forceNewSession: true so the backend creates a fresh session on first send", () => {
    useChatStore.getState().setCurrentAgent("Agent2");

    expect(useChatStore.getState().agents["Agent2"]?.forceNewSession).toBe(true);
  });
});

// ── 3. localStorage preservation ────────────────────────────────────────────

describe("setCurrentAgent — localStorage not cleared on switch", () => {
  it("retains the session ID in localStorage after switching to a known agent", () => {
    saveLastSession("Agent2", "sess-agent2-stored");

    useChatStore.getState().setCurrentAgent("Agent2");

    // Must NOT be cleared — next switch back to Agent2 needs it for pre-population
    expect(getLastSessionId("Agent2")).toBe("sess-agent2-stored");
  });

  it("does not clear the session when the agent is visited a second time", () => {
    saveLastSession("Agent2", "sess-agent2-stored");

    useChatStore.getState().setCurrentAgent("Agent2");
    // Switch away and back
    useChatStore.getState().setCurrentAgent("Agent1");
    useChatStore.getState().setCurrentAgent("Agent2");

    expect(getLastSessionId("Agent2")).toBe("sess-agent2-stored");
  });
});

// ── 4. Invariants that must be preserved ────────────────────────────────────

describe("setCurrentAgent — preserved invariants", () => {
  it("is a no-op when switching to the same agent", () => {
    useChatStore.getState().setCurrentAgent("Agent1");

    // State should not change for Agent1 — especially activeSessionId
    expect(useChatStore.getState().agents["Agent1"]?.activeSessionId).toBe("sess-agent1");
  });

  it("resets streamError to null on switch", () => {
    saveLastSession("Agent2", "sess-agent2-stored");
    useChatStore.setState((s) => {
      s.agents["Agent1"] = { ...agentWithSession("sess-agent1"), streamError: "some error" };
    });

    useChatStore.getState().setCurrentAgent("Agent2");

    expect(useChatStore.getState().agents["Agent2"]?.streamError).toBeNull();
  });

  it("sets connectionPhase to idle for the new agent", () => {
    saveLastSession("Agent2", "sess-agent2-stored");

    useChatStore.getState().setCurrentAgent("Agent2");

    expect(useChatStore.getState().agents["Agent2"]?.connectionPhase).toBe("idle");
  });

  it("updates currentAgent to the new agent", () => {
    useChatStore.getState().setCurrentAgent("Agent2");

    expect(useChatStore.getState().currentAgent).toBe("Agent2");
  });
});

// ── 5. Restore-effect validation (static analysis of page.tsx) ───────────────

describe("restore effect — page.tsx validates pre-populated session ID", () => {
  it("checks sessions.some() before marking the pre-populated session as restored", async () => {
    const fs = await import("node:fs");
    const path = await import("node:path");
    const src = fs.readFileSync(
      path.resolve(__dirname, "../../app/(authenticated)/chat/page.tsx"),
      "utf8"
    );

    // The restore-effect guard must validate that the pre-populated session
    // exists in the current sessions list (not just assume it is still valid).
    expect(src).toContain("sessions.some((s) => s.id === agentState.activeSessionId)");
  });

  it("has a fall-through path for stale session IDs (not in current list)", async () => {
    const fs = await import("node:fs");
    const path = await import("node:path");
    const src = fs.readFileSync(
      path.resolve(__dirname, "../../app/(authenticated)/chat/page.tsx"),
      "utf8"
    );

    // After the sessions.some() guard there must be a comment explaining the
    // fall-through (deleted / outside top-40 sessions) — not an unconditional
    // early return that would leave the user on a stale session forever.
    const validationIdx = src.indexOf(
      "sessions.some((s) => s.id === agentState.activeSessionId)"
    );
    expect(validationIdx).toBeGreaterThan(-1);

    // The block between the validation and Priority 1 must NOT contain an
    // unconditional early return — the fall-through path must reach sessions[0].
    const toP1 = src.slice(validationIdx, src.indexOf("// Priority 1: URL ?s= param"));
    // There must be an else/fall-through path (not just a hard `return`)
    expect(toP1).toMatch(/fall\s+through/i);
  });
});

// ── 7. Override-state contract (static analysis of page.tsx) ─────────────────

describe("overrideUrlSession override-state contract", () => {
  async function getPageSrc() {
    const fs = await import("node:fs");
    const path = await import("node:path");
    return fs.readFileSync(
      path.resolve(__dirname, "../../app/(authenticated)/chat/page.tsx"),
      "utf8"
    );
  }

  it("declares overrideUrlSession state", async () => {
    const src = await getPageSrc();
    expect(src).toContain("overrideUrlSession");
    expect(src).toContain("setOverrideUrlSession");
  });

  it("declares effectiveUrlSessionId derived from override", async () => {
    const src = await getPageSrc();
    expect(src).toContain("effectiveUrlSessionId");
    expect(src).toContain(
      "overrideUrlSession !== undefined ? overrideUrlSession : urlSessionId"
    );
  });

  it("switchAgent sets override to null and does NOT call router.replace", async () => {
    const src = await getPageSrc();
    const switchBlock = src.slice(
      src.indexOf("const switchAgent = useCallback"),
      src.indexOf("}, []);", src.indexOf("const switchAgent = useCallback")) + "}, []);".length
    );
    expect(switchBlock).toContain("setOverrideUrlSession(null)");
    expect(switchBlock).not.toContain("router.replace");
  });

  it("cross-agent resolver uses effectiveUrlSessionId in body and deps", async () => {
    const src = await getPageSrc();
    const resolverBlock = src.slice(
      src.indexOf("urlResolveFetched = useRef"),
      src.indexOf("}, [effectiveUrlSessionId,") + "}, [effectiveUrlSessionId,".length
    );
    expect(resolverBlock).toContain("effectiveUrlSessionId");
    expect(resolverBlock).toContain("[effectiveUrlSessionId,");
  });

  it("URL-sync guard uses effectiveUrlSessionId", async () => {
    const src = await getPageSrc();
    const syncBlock = src.slice(
      src.indexOf("// Sync activeSessionId → URL ?s= param"),
      src.indexOf("}, [activeSessionId, searchParams, sessions, effectiveUrlSessionId]);")
        + "}, [activeSessionId, searchParams, sessions, effectiveUrlSessionId]);".length
    );
    expect(syncBlock).toContain("effectiveUrlSessionId");
    expect(syncBlock).toContain("[activeSessionId, searchParams, sessions, effectiveUrlSessionId]");
  });
});
