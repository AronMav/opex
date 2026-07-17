/**
 * Wave-2 Task 13a: one-off model override on regenerate.
 *
 * Backend (T12) accepts an optional `model` field on POST /api/chat — a
 * ONE-OFF override for that single turn, never persisted. This file verifies
 * two layers:
 *
 *  1. streaming-renderer.sendTurn actually builds `body.model` when passed an
 *     `opts.model`, and omits the key entirely otherwise (real POST body).
 *  2. The store-level regenerate() / regenerateFrom() actions thread an
 *     `{ model }` opts object through forkAndStream all the way to
 *     renderer.sendTurn, and a subsequent PLAIN send never carries a model
 *     (no leaked/persisted override).
 *
 * Each section uses `vi.doMock` + `vi.resetModules()` (rather than one
 * file-wide hoisted `vi.mock`) because section 1 needs the REAL
 * streaming-renderer module while section 2 needs it mocked.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

describe("streaming-renderer.sendTurn — body.model (13a)", () => {
  beforeEach(() => {
    vi.resetModules();
  });

  it("includes body.model when opts.model is passed, omits it otherwise", async () => {
    const startTurnMock = vi.fn().mockResolvedValue({ session_id: "s1", user_message_id: "u1" });
    vi.doMock("@/stores/stream/chat-stream", () => ({
      startTurn: startTurnMock,
      openTurnStream: vi.fn(),
    }));
    vi.doMock("@/stores/stream-session", () => ({
      streamSessionManager: {
        start: () => ({ signal: { aborted: false }, isCurrent: () => true }),
        disposeCurrent: vi.fn(),
      },
    }));
    vi.doMock("@/lib/query-client", () => ({
      queryClient: {
        invalidateQueries: vi.fn(),
        getQueriesData: vi.fn(() => []),
      },
    }));
    vi.doMock("@/lib/api", () => ({
      apiPatch: vi.fn().mockResolvedValue({}),
      apiPost: vi.fn().mockResolvedValue({}),
    }));
    vi.doMock("@/stores/chat-history", () => ({
      getCachedRawMessages: vi.fn(() => []),
      resolveActivePath: vi.fn(() => []),
    }));

    const { createStreamingRenderer } = await import("@/stores/streaming-renderer");
    const { emptyAgentState } = await import("@/stores/chat-types");
    type ChatStore = import("@/stores/chat-types").ChatStore;
    type StoreAccess = import("@/stores/streaming-renderer").StoreAccess;

    const AGENT = "main";
    const state = { agents: { [AGENT]: emptyAgentState() } } as unknown as ChatStore;
    const access: StoreAccess = {
      get: () => state,
      set: (fn) => fn(state),
    };
    const r = createStreamingRenderer(access);

    await r.sendTurn(AGENT, "s1", "hello", { model: "gpt-x" });
    expect(startTurnMock).toHaveBeenCalledTimes(1);
    expect(startTurnMock.mock.calls[0][1]).toMatchObject({ model: "gpt-x" });

    await r.sendTurn(AGENT, "s1", "a plain follow-up");
    expect(startTurnMock).toHaveBeenCalledTimes(2);
    expect(startTurnMock.mock.calls[1][1]).not.toHaveProperty("model");

    r.dispose();
  });
});

describe("regenerate / regenerateFrom — model threaded through to sendTurn (13a)", () => {
  const AGENT = "main";
  const U1 = { id: "u1", role: "user" as const, parts: [{ type: "text" as const, text: "hi" }], createdAt: "" };
  const A1 = { id: "a1", role: "assistant" as const, parts: [{ type: "text" as const, text: "yo" }], createdAt: "" };

  beforeEach(() => {
    vi.resetModules();
  });

  async function setup() {
    const sendTurnMock = vi.fn();
    vi.doMock("@/stores/streaming-renderer", () => ({
      createStreamingRenderer: () => ({
        sendTurn: sendTurnMock,
        connect: vi.fn(),
        resumeStream: vi.fn(),
        abortActiveStream: vi.fn(),
        abortLocalOnly: vi.fn(),
        cleanupAgent: vi.fn(),
        dispose: vi.fn(),
        onSessionId: vi.fn(),
      }),
    }));
    const apiPostMock = vi.fn().mockResolvedValue({
      message_id: "b1",
      parent_message_id: "p1",
      branch_from_message_id: "u1",
    });
    vi.doMock("@/lib/api", () => ({
      apiGet: vi.fn().mockResolvedValue({}),
      apiPost: apiPostMock,
      apiPut: vi.fn().mockResolvedValue({}),
      apiPatch: vi.fn().mockResolvedValue({}),
      apiDelete: vi.fn().mockResolvedValue(undefined),
      getToken: () => "t",
      assertToken: () => "t",
      handleUnauthorized: vi.fn(),
    }));
    vi.doMock("@/lib/query-client", () => ({
      queryClient: {
        invalidateQueries: vi.fn(),
        setQueryData: vi.fn(),
        getQueryData: vi.fn(() => undefined),
        getQueriesData: vi.fn(() => []),
        refetchQueries: vi.fn().mockResolvedValue(undefined),
      },
    }));

    const { useChatStore } = await import("@/stores/chat-store");
    const { emptyAgentState } = await import("@/stores/chat-types");

    useChatStore.setState({
      currentAgent: AGENT,
      agents: {
        [AGENT]: {
          ...emptyAgentState(),
          activeSessionId: "s1",
          connectionPhase: "idle",
          messageSource: { mode: "live", messages: [U1, A1] },
        },
      },
    });

    return { useChatStore, sendTurnMock, apiPostMock };
  }

  const flush = () => new Promise<void>((r) => setTimeout(r, 0));

  it("regenerate({model}) calls sendTurn with the model; plain regenerate() does not", async () => {
    const { useChatStore, sendTurnMock, apiPostMock } = await setup();

    useChatStore.getState().regenerate({ model: "gpt-x" });
    await flush();
    expect(sendTurnMock).toHaveBeenLastCalledWith(AGENT, "s1", "hi", { userMessageId: "b1", model: "gpt-x" });

    sendTurnMock.mockClear();
    apiPostMock.mockResolvedValueOnce({ message_id: "b2", parent_message_id: "p1", branch_from_message_id: "u1" });

    useChatStore.getState().regenerate();
    await flush();
    expect(sendTurnMock).toHaveBeenLastCalledWith(AGENT, "s1", "hi", { userMessageId: "b2", model: undefined });
  });

  it("regenerateFrom(id, {model}) passes the model through the same forkAndStream flow", async () => {
    const { useChatStore, sendTurnMock } = await setup();

    useChatStore.getState().regenerateFrom("u1", { model: "gpt-y" });
    await flush();
    expect(sendTurnMock).toHaveBeenLastCalledWith(AGENT, "s1", "hi", { userMessageId: "b1", model: "gpt-y" });
  });

  it("a subsequent plain sendMessage never carries the previous regenerate's model (no leak/persistence)", async () => {
    const { useChatStore, sendTurnMock } = await setup();

    useChatStore.getState().regenerate({ model: "gpt-x" });
    await flush();
    expect(sendTurnMock).toHaveBeenLastCalledWith(AGENT, "s1", "hi", { userMessageId: "b1", model: "gpt-x" });

    sendTurnMock.mockClear();
    useChatStore.getState().sendMessage("hello again");

    expect(sendTurnMock).toHaveBeenCalledTimes(1);
    const call = sendTurnMock.mock.calls[0];
    expect(call[2]).toBe("hello again");
    expect(call[3]).not.toHaveProperty("model");
  });
});
