/**
 * Direct unit test for the REAL `queueMessage` reducer in stream-control.ts
 * (not a mock reimplementation — this exercises `useChatStore.getState().queueMessage`
 * from the actual store, same pattern as agent-switching.test.ts /
 * session-switch-invalidation.test.ts).
 *
 * Model: `pendingMessage` is a FIFO queue (`PendingMessageEntry[]`) since the
 * "FIFO message queue" redesign — each `queueMessage` call APPENDS an entry;
 * messages accumulate while the model works and drain one-by-one on idle. The
 * old single-object "last intent wins / append-with-\n" semantics are gone.
 *
 * Regression still covered (Fix 1, task-4 review): each entry's `voice` is
 * `opts?.voice === true`, so a PLAIN-TEXT queue call (no `opts` — e.g.
 * Shift+Enter, or the F085 interrupt-race path) always produces a `voice:false`
 * entry and NEVER inherits `voice:true` from an earlier queued voice message —
 * a typed message's reply is never read aloud.
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

const queue = () => useChatStore.getState().agents[AGENT]?.pendingMessage ?? [];

describe("queueMessage — real reducer (stream-control.ts, FIFO queue)", () => {
  it("a fresh voice message queues a single voice:true entry", () => {
    useChatStore.getState().queueMessage("привет", undefined, { voice: true });

    const q = queue();
    expect(q).toHaveLength(1);
    expect(q[0]).toMatchObject({ content: "привет", voice: true });
  });

  it("two voice messages queue two separate FIFO entries (no \\n append)", () => {
    useChatStore.getState().queueMessage("первая фраза", undefined, { voice: true });
    useChatStore.getState().queueMessage("вторая фраза", undefined, { voice: true });

    const q = queue();
    expect(q).toHaveLength(2);
    expect(q[0]).toMatchObject({ content: "первая фраза", voice: true });
    expect(q[1]).toMatchObject({ content: "вторая фраза", voice: true });
  });

  it("a plain-text queue call after a voice message queues a separate voice:false entry (Fix 1)", () => {
    useChatStore.getState().queueMessage("говорю голосом", undefined, { voice: true });

    // No opts — the Shift+Enter path / F085 interrupt-race path call queueMessage
    // this way. The new entry must be voice:false and must NOT inherit voice:true
    // from the earlier queued voice message (so its reply is never read aloud).
    useChatStore.getState().queueMessage("напечатал текст");

    const q = queue();
    expect(q).toHaveLength(2);
    expect(q[0]).toMatchObject({ content: "говорю голосом", voice: true });
    expect(q[1]).toMatchObject({ content: "напечатал текст", voice: false });
  });

  it("opts.voice explicitly false also queues a voice:false entry", () => {
    useChatStore.getState().queueMessage("говорю голосом", undefined, { voice: true });
    useChatStore.getState().queueMessage("напечатал текст", undefined, { voice: false });

    const q = queue();
    expect(q).toHaveLength(2);
    expect(q[1]).toMatchObject({ content: "напечатал текст", voice: false });
  });

  it("a fresh non-voice queue call (no prior pending) queues a voice:false entry", () => {
    useChatStore.getState().queueMessage("plain text");

    const q = queue();
    expect(q).toHaveLength(1);
    expect(q[0]).toMatchObject({ content: "plain text", voice: false });
  });
});
