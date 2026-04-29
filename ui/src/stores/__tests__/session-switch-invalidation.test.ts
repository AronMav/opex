/**
 * Regression test 2026-04-18: when the user switches sessions (either via
 * `selectSession`, `selectSessionById`, `newChat`, or `setCurrentAgent`),
 * the React Query cache for BOTH the departing and incoming session must
 * be invalidated — otherwise returning to a previously-streaming session
 * shows STALE data (the user's initial message can be missing if the
 * cache was populated before the backend saved it).
 *
 * We verify by spying on `queryClient.invalidateQueries` and driving the
 * store through the four entry points.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

// Track invalidation calls across the test suite. `vi.hoisted` runs BEFORE
// the hoisted `vi.mock` factories, so the mock can reference the spy.
const { mockInvalidate } = vi.hoisted(() => ({ mockInvalidate: vi.fn() }));

vi.mock("@/lib/query-client", () => ({
  queryClient: {
    invalidateQueries: mockInvalidate,
    setQueryData: vi.fn(),
    getQueryData: vi.fn(() => undefined),
  },
}));

// Stub the streaming renderer — session switches call `abortActiveStream`
// / `cleanupAgent` as side effects; we don't exercise them here.
vi.mock("@/stores/streaming-renderer", () => ({
  createStreamingRenderer: () => ({
    startStream: vi.fn(),
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

import { useChatStore } from "@/stores/chat-store";

// Match the shape returned by qk.sessionMessages(sid) → ["sessions", sid, "messages"].
// The sessionId sits at index 1; we extract it by position to avoid magic-number
// length heuristics that could break if other key segments happen to be long strings.
function invalidatedSessionIds(): string[] {
  return mockInvalidate.mock.calls
    .map((call) => {
      const arg = call[0] as { queryKey?: unknown[] } | undefined;
      const key = arg?.queryKey;
      if (!Array.isArray(key) || key[0] !== "sessions") return null;
      return typeof key[1] === "string" ? key[1] : null;
    })
    .filter((x): x is string => x !== null);
}

beforeEach(() => {
  mockInvalidate.mockClear();
  // Reset Zustand store via internal API (no explicit reset exported).
  useChatStore.setState({
    currentAgent: "Agent1",
    agents: {},
    sessionParticipants: {},
  });
});

describe("session switch invalidates React Query cache", () => {
  it("selectSession invalidates BOTH outgoing and incoming session", async () => {
    // Start in session A
    const store = useChatStore.getState();
    useChatStore.setState({
      currentAgent: "Agent1",
      agents: {
        Agent1: {
          ...defaultAgentState(),
          activeSessionId: "session-A",
          messageSource: { mode: "live", messages: [] },
          connectionPhase: "idle",
        },
      },
    });
    mockInvalidate.mockClear();

    await store.selectSession("session-B", "Agent1");

    const invalidated = invalidatedSessionIds();
    expect(invalidated).toContain("session-A");
    expect(invalidated).toContain("session-B");
  });

  it("selectSession skips invalidation when re-selecting active streaming session", async () => {
    const store = useChatStore.getState();
    useChatStore.setState({
      currentAgent: "Agent1",
      agents: {
        Agent1: {
          ...defaultAgentState(),
          activeSessionId: "session-A",
          connectionPhase: "streaming",
        },
      },
    });
    mockInvalidate.mockClear();

    await store.selectSession("session-A", "Agent1");

    expect(invalidatedSessionIds()).toEqual([]);
  });

  it("selectSessionById invalidates previous session's cache", () => {
    const store = useChatStore.getState();
    useChatStore.setState({
      currentAgent: "Agent1",
      agents: {
        Agent1: {
          ...defaultAgentState(),
          activeSessionId: "session-A",
        },
      },
    });
    mockInvalidate.mockClear();

    store.selectSessionById("Agent1", "session-B");

    const invalidated = invalidatedSessionIds();
    expect(invalidated).toContain("session-A");
    expect(invalidated).toContain("session-B");
  });

  it("newChat invalidates the departing session's cache", () => {
    const store = useChatStore.getState();
    useChatStore.setState({
      currentAgent: "Agent1",
      agents: {
        Agent1: {
          ...defaultAgentState(),
          activeSessionId: "session-A",
        },
      },
    });
    mockInvalidate.mockClear();

    store.newChat();

    expect(invalidatedSessionIds()).toContain("session-A");
  });

  it("newChat does NOT invalidate anything when there is no active session", () => {
    const store = useChatStore.getState();
    useChatStore.setState({
      currentAgent: "Agent1",
      agents: {
        Agent1: { ...defaultAgentState(), activeSessionId: null },
      },
    });
    mockInvalidate.mockClear();

    store.newChat();

    expect(invalidatedSessionIds()).toEqual([]);
  });

  it("setCurrentAgent to a new agent invalidates the previous agent's active session", () => {
    const store = useChatStore.getState();
    useChatStore.setState({
      currentAgent: "Agent1",
      agents: {
        Agent1: {
          ...defaultAgentState(),
          activeSessionId: "session-A",
        },
      },
      sessionParticipants: {},
    });
    mockInvalidate.mockClear();

    store.setCurrentAgent("Agent2");

    expect(invalidatedSessionIds()).toContain("session-A");
  });

  it("setCurrentAgent to a multi-agent participant invalidates the shared session so the new agent sees fresh data", () => {
    const store = useChatStore.getState();
    useChatStore.setState({
      currentAgent: "Agent1",
      agents: {
        Agent1: {
          ...defaultAgentState(),
          activeSessionId: "multi-session",
        },
      },
      sessionParticipants: {
        "multi-session": ["Agent1", "Agent2"],
      },
    });
    mockInvalidate.mockClear();

    store.setCurrentAgent("Agent2");

    expect(invalidatedSessionIds()).toContain("multi-session");
  });
});

// ── Helpers ─────────────────────────────────────────────────────────────────

// Import the canonical factory so this test cannot drift from AgentState.
// Using the real emptyAgentState() keeps the shape in sync with chat-types.ts
// whenever new fields are added — TypeScript regresses would fail at import.
import { emptyAgentState } from "@/stores/chat-types";
const defaultAgentState = emptyAgentState;
