/**
 * Critical 2 (caret fix wave 2): on abort (user Stop / agent switch), the
 * stream-processor's finally-block commit is a no-op (disposeCurrent bumped
 * the generation → isCurrent false) and the post-finally handoff is gated on
 * `!signal.aborted` — so nothing else settles the live message's status.
 * `abortLocalOnly` must sweep the agent's live/finishing messages and settle
 * every status === "streaming" to "complete", or the caret blinks PERMANENTLY
 * (messageSource stays "live" until manual navigation).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../stream/chat-stream", () => ({
  startTurn: vi.fn().mockResolvedValue({ session_id: "s1", user_message_id: "u1" }),
  openTurnStream: vi.fn(),
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
import type { ChatStore, ChatMessage } from "../chat-types";

const AGENT = "main";

function makeStore() {
  const state = { agents: { [AGENT]: emptyAgentState() } } as unknown as ChatStore;
  const access: StoreAccess = {
    get: () => state,
    set: (fn) => fn(state),
  };
  return { state, access };
}

function liveMessagesOf(state: ChatStore): ChatMessage[] {
  const src = state.agents[AGENT].messageSource;
  return src.mode === "live" || src.mode === "finishing" ? src.messages : [];
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("abortLocalOnly — settle streaming message status (no stuck caret)", () => {
  it("mid-stream abort settles a live \"streaming\" message (status not \"streaming\")", () => {
    const { state, access } = makeStore();
    state.agents[AGENT].connectionPhase = "streaming";
    state.agents[AGENT].messageSource = {
      mode: "live",
      messages: [
        { id: "u1", role: "user", parts: [{ type: "text", text: "hi" }], status: "confirmed" },
        { id: "a1", role: "assistant", parts: [{ type: "text", text: "partial" }], status: "streaming" },
      ],
    };
    const r = createStreamingRenderer(access);
    r.abortLocalOnly(AGENT);
    r.dispose();

    const msgs = liveMessagesOf(state);
    const assistant = msgs.find((m) => m.id === "a1");
    expect(assistant?.status).not.toBe("streaming");
    // user message untouched
    expect(msgs.find((m) => m.id === "u1")?.status).toBe("confirmed");
  });

  it("abort sweeps ALL streaming messages (multi-iteration turn)", () => {
    const { state, access } = makeStore();
    state.agents[AGENT].connectionPhase = "streaming";
    state.agents[AGENT].messageSource = {
      mode: "live",
      messages: [
        { id: "iter-0", role: "assistant", parts: [], status: "streaming" },
        { id: "iter-1", role: "assistant", parts: [], status: "streaming" },
      ],
    };
    const r = createStreamingRenderer(access);
    r.abortLocalOnly(AGENT);
    r.dispose();

    for (const msg of liveMessagesOf(state)) {
      expect(msg.status, `message ${msg.id}`).toBe("complete");
    }
  });

  it("connect() pre-open cleanup does NOT sweep — reconnect keeps the caret alive", () => {
    // Important (re-review): connection-drop → onConnectionLost →
    // scheduleReconnect → connect(), whose FIRST statement is the
    // abortLocalOnly cleanup. If that cleanup sweeps, the still-streaming
    // message is marked "complete" and the no-downgrade guards (sync's
    // `status !== "complete"` and commit()'s guard) make it PERMANENT: after
    // any transient drop / visibility-stale recovery, text keeps appending
    // but the caret never returns. The pre-open cleanup is a reconnect
    // CONTINUATION, not a teardown — status must stay "streaming" until the
    // envelope settles it.
    const { state, access } = makeStore();
    state.agents[AGENT].connectionPhase = "streaming";
    state.agents[AGENT].messageSource = {
      mode: "live",
      messages: [
        { id: "a1", role: "assistant", parts: [{ type: "text", text: "partial" }], status: "streaming" },
      ],
    };
    const r = createStreamingRenderer(access);
    r.connect(AGENT, "s1");
    r.dispose();

    const msgs = liveMessagesOf(state);
    expect(msgs.find((m) => m.id === "a1")?.status).toBe("streaming");
  });

  it("abortActiveStream (user Stop) still sweeps — intentional teardown", () => {
    const { state, access } = makeStore();
    state.agents[AGENT].connectionPhase = "streaming";
    state.agents[AGENT].activeSessionId = "s1";
    state.agents[AGENT].messageSource = {
      mode: "live",
      messages: [
        { id: "a1", role: "assistant", parts: [{ type: "text", text: "partial" }], status: "streaming" },
      ],
    };
    const r = createStreamingRenderer(access);
    r.abortActiveStream(AGENT);
    r.dispose();

    expect(liveMessagesOf(state).find((m) => m.id === "a1")?.status).toBe("complete");
  });

  it("abort sweep also runs when connectionPhase is already idle (dispose landed first)", () => {
    // disposeCurrent → dispose() writes connectionPhase "idle" BEFORE the
    // defensive phase-reset check in abortLocalOnly — the sweep must not be
    // gated on an active phase or it never runs in the common Stop flow.
    const { state, access } = makeStore();
    state.agents[AGENT].connectionPhase = "idle";
    state.agents[AGENT].messageSource = {
      mode: "live",
      messages: [
        { id: "a1", role: "assistant", parts: [], status: "streaming" },
      ],
    };
    const r = createStreamingRenderer(access);
    r.abortLocalOnly(AGENT);
    r.dispose();

    expect(liveMessagesOf(state)[0]?.status).toBe("complete");
  });
});
