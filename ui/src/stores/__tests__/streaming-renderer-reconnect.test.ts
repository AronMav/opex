/**
 * Fix I: `onConnectionLost` must NOT tight-loop. It schedules a backed-off
 * reconnect (setTimeout, exponential) and, after a retry cap, STOPS and
 * surfaces a visible error state instead of hammering a persistently failing
 * GET. A successful envelope resets the budget.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Capture the callbacks the renderer hands to openTurnStream so the test can
// drive onConnectionLost / onEnvelopeApplied deterministically.
const { openTurnStreamMock } = vi.hoisted(() => ({ openTurnStreamMock: vi.fn() }));
let lastCb: {
  onEnvelopeApplied: () => void;
  onFinished: () => void;
  onConnectionLost: () => void;
} | null = null;

vi.mock("../stream/chat-stream", () => ({
  startTurn: vi.fn().mockResolvedValue({ session_id: "s1", user_message_id: "u1" }),
  openTurnStream: (
    _agent: string,
    _sid: string,
    _session: unknown,
    cb: NonNullable<typeof lastCb>,
  ) => {
    lastCb = cb;
    openTurnStreamMock();
  },
}));

vi.mock("../stream-session", () => ({
  streamSessionManager: {
    start: () => ({ signal: { aborted: false }, isCurrent: () => true }),
    disposeCurrent: vi.fn(),
    // `current` returns a truthy object so scheduleReconnect's disposed-check
    // passes and the reconnect logic is exercised. Tests that specifically
    // need the disposed branch can override this mock.
    current: () => ({ signal: { aborted: false }, isCurrent: () => true }),
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

function makeStore() {
  const state = { agents: { [AGENT]: emptyAgentState() } } as unknown as ChatStore;
  const access: StoreAccess = {
    get: () => state,
    set: (fn) => fn(state),
  };
  return { state, access };
}

beforeEach(() => {
  vi.useFakeTimers();
  openTurnStreamMock.mockClear();
  lastCb = null;
});
afterEach(() => {
  vi.useRealTimers();
});

describe("Fix I — bounded reconnect", () => {
  it("does NOT reconnect synchronously on a drop; shows reconnecting, then reconnects after backoff", () => {
    const { state, access } = makeStore();
    const r = createStreamingRenderer(access);

    r.connect(AGENT, "s1");
    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);

    // Drop without terminal signal.
    lastCb!.onConnectionLost();
    // No immediate re-open (the old bug tight-looped here).
    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);
    expect(state.agents[AGENT].isLlmReconnecting).toBe(true);
    expect(state.agents[AGENT].connectionPhase).toBe("submitted");

    // Nothing before the base delay.
    vi.advanceTimersByTime(499);
    expect(openTurnStreamMock).toHaveBeenCalledTimes(1);
    // Base delay (500ms) elapses → one reconnect.
    vi.advanceTimersByTime(1);
    expect(openTurnStreamMock).toHaveBeenCalledTimes(2);

    r.dispose();
  });

  it("stops after the retry cap and surfaces a visible error state", () => {
    const { state, access } = makeStore();
    const r = createStreamingRenderer(access);

    r.connect(AGENT, "s1");
    // 6 retries are allowed; the 7th drop trips the cap.
    for (let i = 0; i < 6; i++) {
      lastCb!.onConnectionLost();
      vi.advanceTimersByTime(RECONNECT_MAX_DELAY); // large enough for any backoff step
    }
    // 1 initial + 6 reconnects.
    expect(openTurnStreamMock).toHaveBeenCalledTimes(7);
    expect(state.agents[AGENT].connectionPhase).not.toBe("error");

    // 7th drop → give up.
    lastCb!.onConnectionLost();
    vi.advanceTimersByTime(RECONNECT_MAX_DELAY);
    expect(state.agents[AGENT].connectionPhase).toBe("error");
    expect(state.agents[AGENT].connectionError).toBe("reconnect-failed");
    expect(state.agents[AGENT].isLlmReconnecting).toBe(false);
    // No further reconnect attempts after the cap.
    expect(openTurnStreamMock).toHaveBeenCalledTimes(7);

    r.dispose();
  });

  it("settles live/finishing \"streaming\" messages to \"complete\" on give-up (Item 2 — no stuck caret)", () => {
    // Give-up leaves connectionPhase "error" but previously never touched
    // message status — the drop path deliberately preserves "streaming" so a
    // reconnect can keep appending text. Once we've truly given up (no more
    // reconnects will ever be attempted), the caret must stop blinking under
    // the error banner, same idiom as abortLocalOnly's sweep.
    const { state, access } = makeStore();
    state.agents[AGENT].messageSource = {
      mode: "live",
      messages: [
        { id: "a1", role: "assistant", parts: [{ type: "text", text: "partial" }], status: "streaming" },
      ],
    };
    const r = createStreamingRenderer(access);

    r.connect(AGENT, "s1");
    for (let i = 0; i < 6; i++) {
      lastCb!.onConnectionLost();
      vi.advanceTimersByTime(RECONNECT_MAX_DELAY);
    }
    // 7th drop → give up.
    lastCb!.onConnectionLost();
    vi.advanceTimersByTime(RECONNECT_MAX_DELAY);

    expect(state.agents[AGENT].connectionPhase).toBe("error");
    const src = state.agents[AGENT].messageSource;
    const msgs = src.mode === "live" || src.mode === "finishing" ? src.messages : [];
    expect(msgs.find((m) => m.id === "a1")?.status).toBe("complete");

    r.dispose();
  });

  it("a successful envelope resets the reconnect budget", () => {
    const { state, access } = makeStore();
    const r = createStreamingRenderer(access);

    r.connect(AGENT, "s1");
    // Burn 3 retries.
    for (let i = 0; i < 3; i++) {
      lastCb!.onConnectionLost();
      vi.advanceTimersByTime(RECONNECT_MAX_DELAY);
    }
    // A healthy envelope commits → budget resets.
    lastCb!.onEnvelopeApplied();
    expect(state.agents[AGENT].connectionPhase).toBe("streaming");

    // Now 6 more drops should still be allowed (budget was reset), i.e. no
    // error before the fresh cap.
    for (let i = 0; i < 6; i++) {
      lastCb!.onConnectionLost();
      vi.advanceTimersByTime(RECONNECT_MAX_DELAY);
    }
    expect(state.agents[AGENT].connectionPhase).not.toBe("error");

    r.dispose();
  });
});

// Matches RECONNECT_MAX_DELAY_MS in streaming-renderer.ts.
const RECONNECT_MAX_DELAY = 15_000;
