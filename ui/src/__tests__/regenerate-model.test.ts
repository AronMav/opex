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
        current: () => ({ signal: { aborted: false }, isCurrent: () => true }),
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
    // WS2: regenerate() now unconditionally consults getCachedHistoryMessages
    // (to find a persisted fallback anchor past the live overlay) — mock it
    // explicitly here (empty history) so a later test file section's mock of
    // this same module (registered via vi.doMock, which is NOT undone by
    // vi.resetModules()) can't leak into this describe block's tests.
    vi.doMock("@/stores/chat-history", () => ({
      getCachedHistoryMessages: vi.fn(() => []),
      getCachedRawMessages: vi.fn(() => []),
      resolveActivePath: vi.fn(() => []),
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

/**
 * WS2: «Повторить» / regenerate must never hand the fork endpoint a
 * client-only (never-persisted) message id. A user message's optimistic echo
 * is "sending" until the POST succeeds and "failed" if the POST itself
 * errored — neither is guaranteed to exist server-side. Task 3 (server) added
 * a fallback for an unknown branch id (falls back to the session's last
 * persisted message) so a bad id never 500s — but that fallback ignores which
 * branch the client is actually viewing, so the CLIENT should still prefer a
 * known-persisted anchor when it can find one.
 */
describe("regenerate — persisted branch id resolution (WS2)", () => {
  const AGENT = "main";
  const flush = () => new Promise<void>((r) => setTimeout(r, 0));

  beforeEach(() => {
    vi.resetModules();
  });

  async function setup(historyRows: import("@/stores/chat-types").ChatMessage[]) {
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
      parent_message_id: "u0",
      branch_from_message_id: "u0",
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
    // Directly control the persisted-history side of the lookup — the live
    // overlay in messageSource only ever holds the CURRENT turn (sendTurn
    // replaces it wholesale), so a fallback anchor from an earlier turn can
    // only come from here.
    vi.doMock("@/stores/chat-history", () => ({
      getCachedHistoryMessages: vi.fn(() => historyRows),
      getCachedRawMessages: vi.fn(() => []),
      resolveActivePath: vi.fn(() => []),
    }));

    const { useChatStore } = await import("@/stores/chat-store");
    const { emptyAgentState } = await import("@/stores/chat-types");

    return { useChatStore, emptyAgentState, sendTurnMock, apiPostMock };
  }

  it("walks back to the last PERSISTED user message when the live leaf is an unconfirmed optimistic echo", async () => {
    const U0 = { id: "u0", role: "user" as const, parts: [{ type: "text" as const, text: "first" }], createdAt: "" };
    const A0 = { id: "a0", role: "assistant" as const, parts: [{ type: "text" as const, text: "reply" }], createdAt: "" };
    const { useChatStore, emptyAgentState, sendTurnMock, apiPostMock } = await setup([U0, A0]);

    // Mirrors the real post-sendTurn shape after a failed POST: mode "live"
    // replaces messages with JUST the optimistic echo, never confirmed.
    const U1 = {
      id: "client-only-u1",
      role: "user" as const,
      parts: [{ type: "text" as const, text: "retry me" }],
      createdAt: "",
      status: "failed" as const,
    };

    useChatStore.setState({
      currentAgent: AGENT,
      agents: {
        [AGENT]: {
          ...emptyAgentState(),
          activeSessionId: "s1",
          connectionPhase: "idle",
          messageSource: { mode: "live", messages: [U1] },
        },
      },
    });

    useChatStore.getState().regenerate();
    await flush();

    // Branch anchor must be the PERSISTED u0, never the client-only leaf id.
    expect(apiPostMock).toHaveBeenCalledWith(
      "/api/sessions/s1/fork?agent=main",
      { branch_from_message_id: "u0", content: "retry me" },
    );
    expect(sendTurnMock).toHaveBeenCalledWith(AGENT, "s1", "retry me", { userMessageId: "b1", model: undefined });
  });

  it("omits the fork call and sends a plain turn when no persisted message exists at all", async () => {
    const { useChatStore, emptyAgentState, sendTurnMock, apiPostMock } = await setup([]);

    const U1 = {
      id: "client-only-u1",
      role: "user" as const,
      parts: [{ type: "text" as const, text: "first ever message" }],
      createdAt: "",
      status: "sending" as const,
    };

    useChatStore.setState({
      currentAgent: AGENT,
      agents: {
        [AGENT]: {
          ...emptyAgentState(),
          activeSessionId: "s1",
          connectionPhase: "idle",
          messageSource: { mode: "live", messages: [U1] },
        },
      },
    });

    useChatStore.getState().regenerate();
    await flush();

    expect(apiPostMock).not.toHaveBeenCalled();
    expect(sendTurnMock).toHaveBeenCalledTimes(1);
    expect(sendTurnMock).toHaveBeenCalledWith(AGENT, "s1", "first ever message", {
      userMessageId: expect.any(String),
      model: undefined,
    });
  });
});

/**
 * WS2b: sending a message while viewing a terminal (failed/interrupted/done)
 * session must KEEP that session_id in the POST — the server's ExplicitResume
 * re-entry handles reviving it. selectSession()/selectSessionById() always
 * set forceNewSession: false when opening ANY existing session regardless of
 * its run_status (only the explicit "New Chat" button / a brand-new agent
 * sets it true) — this proves sendMessage's POST body reflects that: no
 * silent new-session creation from the composer.
 */
describe("sendMessage — keeps session_id when reviving a terminal session (WS2b)", () => {
  const AGENT = "main";
  const flush = () => new Promise<void>((r) => setTimeout(r, 0));

  beforeEach(() => {
    vi.resetModules();
  });

  it.each(["failed", "interrupted", "done"] as const)(
    "POSTs with the existing session_id (no force_new_session) when the active session's last turn is %s",
    async () => {
      // This describe block needs the REAL streaming-renderer (to exercise
      // the actual body-construction it POSTs) — an earlier describe block in
      // this file registers a stub via vi.doMock, which vi.resetModules()
      // does NOT undo (only the module cache is cleared, not queued mocks).
      // Explicitly unmock it so this dynamic import resolves the real module.
      vi.doUnmock("@/stores/streaming-renderer");

      const startTurnMock = vi.fn().mockResolvedValue({ session_id: "s-terminal" });
      vi.doMock("@/stores/stream/chat-stream", () => ({
        startTurn: startTurnMock,
        openTurnStream: vi.fn(),
      }));
      vi.doMock("@/stores/stream-session", () => ({
        streamSessionManager: {
          start: () => ({ signal: { aborted: false }, isCurrent: () => true }),
          disposeCurrent: vi.fn(),
          current: () => ({ signal: { aborted: false }, isCurrent: () => true }),
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

      const { useChatStore } = await import("@/stores/chat-store");
      const { emptyAgentState } = await import("@/stores/chat-types");

      // Simulates the state selectSession()/selectSessionById() leaves behind
      // after the user opens an existing (here: terminal) session: activeSessionId
      // set, forceNewSession false, messageSource "history" — regardless of
      // that session's run_status, which navigation.ts never inspects.
      useChatStore.setState({
        currentAgent: AGENT,
        agents: {
          [AGENT]: {
            ...emptyAgentState(),
            activeSessionId: "s-terminal",
            connectionPhase: "idle",
            forceNewSession: false,
            messageSource: { mode: "history", sessionId: "s-terminal" },
          },
        },
      });

      useChatStore.getState().sendMessage("let's continue");
      await flush();

      expect(startTurnMock).toHaveBeenCalledTimes(1);
      const body = startTurnMock.mock.calls[0][1] as Record<string, unknown>;
      expect(body.session_id).toBe("s-terminal");
      expect(body).not.toHaveProperty("force_new_session");
    },
  );
});
