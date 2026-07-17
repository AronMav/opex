/**
 * use-session-restore — deep-link resolver.
 *
 * I1 regression: the cross-agent resolver previously discarded the SAME-agent
 * case (`targetAgent === currentAgent` early-return). When ?s= pointed at a
 * session OUTSIDE the current agent's loaded window (e.g. Ctrl+K from a
 * non-chat page), the restore effect's "already viewing" guard then stranded
 * the user on the OLD session. The fix selects the URL session in place — no
 * agent switch — while the cross-agent path still switches AND selects.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import type { SessionRow } from "@/types/api";

const { selectSession, setCurrentAgent, agentsState } = vi.hoisted(() => ({
  selectSession: vi.fn(),
  setCurrentAgent: vi.fn(),
  agentsState: { current: {} as Record<string, unknown> },
}));

let mockSearch = "";
vi.mock("next/navigation", () => ({
  useSearchParams: () => new URLSearchParams(mockSearch),
}));

vi.mock("@/stores/chat-store", () => ({
  useChatStore: {
    getState: () => ({
      agents: agentsState.current,
      selectSession,
      setCurrentAgent,
      newChat: vi.fn(),
      markSessionActive: vi.fn(),
    }),
  },
  isActivePhase: () => false,
  getInitialAgent: vi.fn(() => "Agent1"),
}));

vi.mock("@/lib/api", () => ({ assertToken: () => "t" }));

import { useSessionRestore } from "../use-session-restore";

function sess(id: string): SessionRow {
  return { id } as SessionRow;
}

beforeEach(() => {
  selectSession.mockClear();
  setCurrentAgent.mockClear();
  // Current agent is viewing "old-sess" (a real history session).
  agentsState.current = {
    Agent1: {
      activeSessionId: "old-sess",
      messageSource: { mode: "history", sessionId: "old-sess" },
      connectionPhase: "idle",
    },
  };
  mockSearch = "s=url-sess";
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    json: () => Promise.resolve({ agent_id: "Agent1" }),
  }) as unknown as typeof fetch;
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useSessionRestore — same-agent deep-link (I1)", () => {
  it("selects the URL session in place when it belongs to the current agent (no setCurrentAgent)", async () => {
    // ?s=url-sess is NOT in the current agent's loaded window, so the resolver
    // fetches its owning agent (= the current agent) and must select it here —
    // otherwise the user is stranded on old-sess.
    renderHook(() =>
      useSessionRestore({
        currentAgent: "Agent1",
        sessions: [sess("old-sess")], // url-sess deliberately absent from the window
        sessionsReady: true,
        activeSessionId: "old-sess",
        agents: ["Agent1", "Agent2"],
      }),
    );

    await waitFor(() =>
      expect(selectSession).toHaveBeenCalledWith("url-sess", "Agent1"),
    );
    // Same agent — no agent switch.
    expect(setCurrentAgent).not.toHaveBeenCalled();
  });

  it("still switches AND selects when the URL session belongs to a DIFFERENT agent", async () => {
    (global.fetch as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({ agent_id: "Agent2" }),
    });

    renderHook(() =>
      useSessionRestore({
        currentAgent: "Agent1",
        sessions: [sess("old-sess")],
        sessionsReady: true,
        activeSessionId: "old-sess",
        agents: ["Agent1", "Agent2"],
      }),
    );

    await waitFor(() =>
      expect(setCurrentAgent).toHaveBeenCalledWith("Agent2"),
    );
    expect(selectSession).toHaveBeenCalledWith("url-sess", "Agent2");
  });

  // I1-b: ?s= points at a session PRESENT in the loaded window while the agent
  // is already viewing a DIFFERENT session. The resolver defers ("restore
  // effect handles this"), so the restore effect's explicit-deep-link branch
  // must beat "already viewing" — previously that branch marked restored +
  // returned before Priority 1 was reached and URL-sync rewrote ?s= back to
  // the old session.
  it("selects the ?s= session over an already-viewed one when it IS in the loaded window (I1-b)", async () => {
    renderHook(() =>
      useSessionRestore({
        currentAgent: "Agent1",
        sessions: [sess("old-sess"), sess("url-sess")], // url-sess IS in the window
        sessionsReady: true,
        activeSessionId: "old-sess",
        agents: ["Agent1", "Agent2"],
      }),
    );

    await waitFor(() =>
      expect(selectSession).toHaveBeenCalledWith("url-sess", "Agent1"),
    );
    // In-window deep-link — the cross-agent resolver never needed to fetch.
    expect(global.fetch).not.toHaveBeenCalled();
    expect(setCurrentAgent).not.toHaveBeenCalled();
  });

  it("keeps current behavior when ?s= equals the already-active session (no extra selectSession)", () => {
    mockSearch = "s=old-sess";
    renderHook(() =>
      useSessionRestore({
        currentAgent: "Agent1",
        sessions: [sess("old-sess")],
        sessionsReady: true,
        activeSessionId: "old-sess",
        agents: ["Agent1", "Agent2"],
      }),
    );

    // Same id → falls through to "already viewing" (marks restored, no re-select).
    expect(selectSession).not.toHaveBeenCalled();
    expect(setCurrentAgent).not.toHaveBeenCalled();
  });
});
