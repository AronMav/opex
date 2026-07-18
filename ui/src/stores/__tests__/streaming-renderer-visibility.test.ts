/**
 * G4: hiding a tab must NEVER dispose or settle a live stream. The hidden
 * branch of the visibility handler is a strict no-op. Returning to the tab
 * (visible / online / pageshow) funnels into ONE staleness-checked reattach
 * that calls the existing single `connect(agent, sessionId)` path — which
 * internally re-opens with settleMessages:false (a reconnect CONTINUATION,
 * not a teardown) so the streaming message id and status survive in place.
 *
 * Mirrors the mocking style of streaming-renderer-reconnect.test.ts.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const { openTurnStreamMock } = vi.hoisted(() => ({ openTurnStreamMock: vi.fn() }));

vi.mock("../stream/chat-stream", () => ({
  startTurn: vi.fn().mockResolvedValue({ session_id: "s1", user_message_id: "u1" }),
  openTurnStream: (..._args: unknown[]) => {
    openTurnStreamMock();
  },
}));

vi.mock("../stream-session", () => ({
  streamSessionManager: {
    start: () => ({ signal: { aborted: false }, isCurrent: () => true }),
    disposeCurrent: vi.fn(),
  },
}));

vi.mock("@/lib/query-client", () => ({
  queryClient: {
    invalidateQueries: vi.fn(),
    getQueriesData: vi.fn(() => []),
  },
}));

vi.mock("@/lib/api", () => ({
  apiPatch: vi.fn().mockResolvedValue({}),
  apiPost: vi.fn().mockResolvedValue({}),
}));

vi.mock("../chat-history", () => ({
  getCachedRawMessages: vi.fn(() => []),
  resolveActivePath: vi.fn(() => []),
}));

import { createStreamingRenderer } from "../streaming-renderer";
import type { StoreAccess } from "../streaming-renderer";
import { emptyAgentState } from "../chat-types";
import type { ChatStore } from "../chat-types";

const AGENT = "main";
const VISIBILITY_STALE_MS = 15_000;

function makeStore() {
  const state = { agents: { [AGENT]: emptyAgentState() } } as unknown as ChatStore;
  const access: StoreAccess = {
    get: () => state,
    set: (fn) => fn(state),
  };
  return { state, access };
}

function setVisibility(hidden: boolean) {
  Object.defineProperty(document, "visibilityState", {
    value: hidden ? "hidden" : "visible",
    configurable: true,
  });
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  vi.useFakeTimers();
  openTurnStreamMock.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
  // Restore a sane default so other test files aren't affected.
  Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
});

describe("visibility lifecycle — hidden branch is a strict no-op (G4)", () => {
  it("document.hidden===true touches NOTHING: no dispose, no settle, no phase change", () => {
    const { state, access } = makeStore();
    state.agents[AGENT].messageSource = {
      mode: "live",
      messages: [
        { id: "a1", role: "assistant", parts: [{ type: "text", text: "partial" }], status: "streaming" },
      ],
    };
    const r = createStreamingRenderer(access);
    r.connect(AGENT, "s1");
    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);

    const phaseBefore = state.agents[AGENT].connectionPhase;
    const messagesBefore = JSON.stringify(state.agents[AGENT].messageSource);

    // Advance well past the staleness window while hidden — must still be a no-op.
    vi.advanceTimersByTime(VISIBILITY_STALE_MS + 1000);
    setVisibility(true);

    expect(openTurnStreamMock).toHaveBeenCalledTimes(1); // no reconnect attempted
    expect(state.agents[AGENT].connectionPhase).toBe(phaseBefore);
    expect(JSON.stringify(state.agents[AGENT].messageSource)).toBe(messagesBefore);
    // Streaming message id + status untouched.
    const src = state.agents[AGENT].messageSource;
    const msgs = src.mode === "live" || src.mode === "finishing" ? src.messages : [];
    expect(msgs.find((m) => m.id === "a1")?.status).toBe("streaming");

    r.dispose();
  });

  it("returning visible after the staleness window reattaches via connect({settleMessages:false}), same message id", () => {
    const { state, access } = makeStore();
    state.agents[AGENT].activeSessionId = "s1";
    state.agents[AGENT].messageSource = {
      mode: "live",
      messages: [
        { id: "a1", role: "assistant", parts: [{ type: "text", text: "partial" }], status: "streaming" },
      ],
    };
    const r = createStreamingRenderer(access);
    r.connect(AGENT, "s1");
    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);

    setVisibility(true);
    vi.advanceTimersByTime(VISIBILITY_STALE_MS + 1000);
    setVisibility(false); // becomes visible again, stream considered stale

    // connect() re-opened the stream (2nd openTurnStream call).
    expect(openTurnStreamMock).toHaveBeenCalledTimes(2);
    // The streaming message must still be present with the SAME id and NOT
    // force-settled to "complete" — reconnect is a continuation, not a reset.
    const src = state.agents[AGENT].messageSource;
    const msgs = src.mode === "live" || src.mode === "finishing" ? src.messages : [];
    expect(msgs.find((m) => m.id === "a1")?.status).toBe("streaming");
  });

  it("does NOT reattach if the tab returns visible before the staleness window elapses", () => {
    const { state, access } = makeStore();
    state.agents[AGENT].activeSessionId = "s1";
    const r = createStreamingRenderer(access);
    r.connect(AGENT, "s1");
    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);

    setVisibility(true);
    vi.advanceTimersByTime(VISIBILITY_STALE_MS - 1000); // still fresh
    setVisibility(false);

    expect(openTurnStreamMock).toHaveBeenCalledTimes(1); // no reattach
    r.dispose();
  });
});

describe("online / pageshow funnel into the SAME staleness-checked reattach", () => {
  it("online event reattaches a stale active stream", () => {
    const { state, access } = makeStore();
    state.agents[AGENT].activeSessionId = "s1";
    const r = createStreamingRenderer(access);
    r.connect(AGENT, "s1");
    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(VISIBILITY_STALE_MS + 1000);
    window.dispatchEvent(new Event("online"));

    expect(openTurnStreamMock).toHaveBeenCalledTimes(2);
    r.dispose();
  });

  it("pageshow event reattaches a stale active stream", () => {
    const { state, access } = makeStore();
    state.agents[AGENT].activeSessionId = "s1";
    const r = createStreamingRenderer(access);
    r.connect(AGENT, "s1");
    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(VISIBILITY_STALE_MS + 1000);
    window.dispatchEvent(new Event("pageshow"));

    expect(openTurnStreamMock).toHaveBeenCalledTimes(2);
    r.dispose();
  });

  it("online/pageshow do NOT reattach a fresh (non-stale) stream", () => {
    const { access } = makeStore();
    const r = createStreamingRenderer(access);
    r.connect(AGENT, "s1");
    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);

    window.dispatchEvent(new Event("online"));
    window.dispatchEvent(new Event("pageshow"));

    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);
    r.dispose();
  });

  it("dispose() removes online and pageshow listeners", () => {
    const { access } = makeStore();
    const removeSpy = vi.spyOn(window, "removeEventListener");
    const r = createStreamingRenderer(access);
    r.dispose();

    const removedTypes = removeSpy.mock.calls.map((c) => c[0]);
    expect(removedTypes).toContain("online");
    expect(removedTypes).toContain("pageshow");
    removeSpy.mockRestore();
  });
});
