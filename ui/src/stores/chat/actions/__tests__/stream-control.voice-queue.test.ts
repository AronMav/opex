/**
 * Direct unit test for the REAL `queueMessage` reducer in stream-control.ts
 * (not a mock reimplementation — this exercises `useChatStore.getState().queueMessage`
 * from the actual store, same pattern as agent-switching.test.ts /
 * session-switch-invalidation.test.ts).
 *
 * Regression covered (Important finding, task-4 review): `queueMessage` used
 * `voice: opts?.voice ?? prev?.voice`, so a PLAIN-TEXT queue call (no `opts` —
 * e.g. Shift+Enter, or the F085 interrupt-race path) made while a voice
 * message was already pending INHERITED `voice: true`, causing a typed
 * message's reply to be read aloud. Fix: `voice: opts?.voice === true` — a
 * non-voice queue call always produces `voice: false` and REPLACES content
 * (last intent wins), never appends onto a prior voice pending message.
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("@/lib/query-client", () => ({
  queryClient: {
    invalidateQueries: vi.fn(),
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

import { useChatStore } from "@/stores/chat-store";
import { emptyAgentState } from "@/stores/chat-types";

const AGENT = "Agent1";

beforeEach(() => {
  useChatStore.setState({
    currentAgent: AGENT,
    agents: { [AGENT]: emptyAgentState() },
  });
});

describe("queueMessage — real reducer (stream-control.ts)", () => {
  it("a fresh voice message sets pendingMessage.voice = true", () => {
    useChatStore.getState().queueMessage("привет", undefined, { voice: true });

    expect(useChatStore.getState().agents[AGENT]?.pendingMessage).toEqual({
      content: "привет",
      attachments: undefined,
      voice: true,
    });
  });

  it("a second voice message appends content with \\n and stays voice: true", () => {
    useChatStore.getState().queueMessage("первая фраза", undefined, { voice: true });
    useChatStore.getState().queueMessage("вторая фраза", undefined, { voice: true });

    expect(useChatStore.getState().agents[AGENT]?.pendingMessage).toEqual({
      content: "первая фраза\nвторая фраза",
      attachments: undefined,
      voice: true,
    });
  });

  it("a plain-text queue call after a pending voice message REPLACES content and sets voice: false (Fix 1)", () => {
    useChatStore.getState().queueMessage("говорю голосом", undefined, { voice: true });

    // No opts — the Shift+Enter path / F085 interrupt-race path call queueMessage
    // this way. Must NOT inherit voice: true from the prior pending message, and
    // must NOT append onto it — a typed message supersedes the queued voice one.
    useChatStore.getState().queueMessage("напечатал текст");

    expect(useChatStore.getState().agents[AGENT]?.pendingMessage).toEqual({
      content: "напечатал текст",
      attachments: undefined,
      voice: false,
    });
  });

  it("opts.voice explicitly false after a pending voice message also replaces and clears the flag", () => {
    useChatStore.getState().queueMessage("говорю голосом", undefined, { voice: true });
    useChatStore.getState().queueMessage("напечатал текст", undefined, { voice: false });

    expect(useChatStore.getState().agents[AGENT]?.pendingMessage).toEqual({
      content: "напечатал текст",
      attachments: undefined,
      voice: false,
    });
  });

  it("a fresh non-voice queue call (no prior pending) sets voice: false", () => {
    useChatStore.getState().queueMessage("plain text");

    expect(useChatStore.getState().agents[AGENT]?.pendingMessage).toEqual({
      content: "plain text",
      attachments: undefined,
      voice: false,
    });
  });
});
