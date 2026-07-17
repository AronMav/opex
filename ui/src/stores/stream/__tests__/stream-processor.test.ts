import { describe, it, expect, vi, beforeEach } from "vitest";
import { processSSEStream } from "../stream-processor";
import { streamSessionManager } from "../../stream-session";
import { useChatStore } from "../../chat-store";
import { getLiveMessages } from "../../chat-types";
import type { MessagePart } from "../../chat-types";

// Build a ReadableStream from an array of frames (SSE "data: ...\n\n" format).
function makeStream(frames: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  let i = 0;
  return new ReadableStream({
    pull(controller) {
      if (i < frames.length) {
        controller.enqueue(encoder.encode(frames[i++]));
      } else {
        controller.close();
      }
    },
  });
}

// Minimal callbacks for processSSEStream.
function makeCallbacks(overrides: Partial<Parameters<typeof processSSEStream>[2]["callbacks"]> = {}) {
  return {
    onSessionId: vi.fn(),
    getAgentState: (agent: string) => useChatStore.getState().agents[agent],
    updateSessionParticipants: vi.fn(),
    ...overrides,
  };
}

beforeEach(() => {
  useChatStore.setState((draft: any) => {
    draft.agents = {
      Arty: {
        activeSessionId: null,
        activeSessionIds: [],
        messageSource: { mode: "new-chat" },
        connectionPhase: "idle",
        connectionError: null,
        streamError: null,
        streamGeneration: 0,
        selectedBranches: {},
        renderLimit: 100,
        turnLimitMessage: null,
        maxReconnectAttempts: 3,
        modelOverride: null,
        forceNewSession: false,
      },
    };
  });
  streamSessionManager.disposeCurrent("Arty");
});

describe("processSSEStream", () => {
  it("invokes onSessionId on first data-session-id frame", async () => {
    const session = streamSessionManager.start("Arty");
    const callbacks = makeCallbacks();
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks,
    });
    expect(callbacks.onSessionId).toHaveBeenCalledWith("s1");
  });

  it("fires onConnectionLost (no settle) when a stream drops mid-turn without finish (T8)", async () => {
    // T8 removed the transport reconnect loop: a drop without a terminal
    // signal (no `finish`, no terminal `sync_begin.runStatus`) fires
    // onConnectionLost and returns early — the caller (streaming-renderer)
    // owns re-open policy, so the module must NOT perform the
    // finishing->history settle that a genuinely-finished turn gets.
    const session = streamSessionManager.start("Arty");
    const onConnectionLost = vi.fn();
    const callbacks = makeCallbacks({ onConnectionLost });
    const frames = [
      `data: ${JSON.stringify({ type: "sync_begin", runStatus: "running", truncated: false })}\n\n`,
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", delta: "hi", id: "t1" })}\n\n`,
      // no sync_end / finish — connection drops mid-turn
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks,
    });
    expect(onConnectionLost).toHaveBeenCalledTimes(1);
    // Early return on connection-loss skips the finishing->history handoff —
    // messageSource must still be "live", never "history".
    expect(useChatStore.getState().agents.Arty.messageSource.mode).toBe("live");
  });

  it("settles cleanly when the stream ends with a finish event", async () => {
    const session = streamSessionManager.start("Arty");
    const callbacks = makeCallbacks();
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "t1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", delta: "hi", id: "t1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end", id: "t1" })}\n\n`,
      `data: ${JSON.stringify({ type: "finish" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks,
    });
    expect(useChatStore.getState().agents.Arty.connectionPhase).toBe("idle");
  });

  it("BUG-A: reasoning parts survive a subsequent tool call", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "start", messageId: "m1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", delta: "<think>deep thought</think>then text" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end" })}\n\n`,
      `data: ${JSON.stringify({ type: "tool-input-start", toolCallId: "t1", toolName: "search" })}\n\n`,
      `data: ${JSON.stringify({ type: "tool-input-available", toolCallId: "t1", input: { q: "x" } })}\n\n`,
      `data: ${JSON.stringify({ type: "finish" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const parts: MessagePart[] = msgs.find(m => m.id === "m1")?.parts ?? [];
    expect(parts.find(p => p.type === "reasoning")).toBeDefined();
    expect(parts.find(p => p.type === "text")).toBeDefined();
    expect(parts.find(p => p.type === "tool")).toBeDefined();
  });

  it("BUG-B: sync error sets connectionPhase to error", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "sync", content: "partial response", status: "error", error: "oops" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    expect(useChatStore.getState().agents.Arty.connectionPhase).toBe("error");
    expect(useChatStore.getState().agents.Arty.streamError).toBe("oops");
  });

  it("REGRESSION (Finding 2): sync_end commit does not blank a DB-branch resume", async () => {
    // Cold refresh of a just-finished turn: the server takes the DB-only
    // resume branch and emits sync_begin -> sync{content} -> sync_end with no
    // text-delta/tool events in between. The `sync` handler writes the
    // resumed content directly into messageSource (bypassing the buffer),
    // tagged with session.buffer.assistantId. Before the fix, sync_end's
    // unconditional commit() found that same message by assistantId and
    // overwrote its parts with the (empty) buffer snapshot, blanking it.
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "sync_begin", runStatus: "finished", truncated: false })}\n\n`,
      `data: ${JSON.stringify({ type: "sync", content: "resumed text", toolCalls: [], status: "finished", error: null })}\n\n`,
      `data: ${JSON.stringify({ type: "sync_end", lastSeq: null })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const assistantMsg = msgs.find((m) => m.role === "assistant");
    expect(assistantMsg).toBeDefined();
    const text = (assistantMsg?.parts ?? [])
      .filter((p): p is Extract<MessagePart, { type: "text" }> => p.type === "text")
      .map((p) => p.text)
      .join("");
    expect(text).toBe("resumed text");
  });

  it("Fix H: syncs a null-stamped pendingMessage.sessionId to the newly-assigned session id (new-chat case)", async () => {
    // A message queued during the "submitted" window of a brand-new chat is
    // stamped sessionId: null (queueMessage reads activeSessionId, which is
    // still null pre-first-byte). Once data-session-id arrives with the
    // turn's real session id, the pending stamp must be updated in the SAME
    // atomic write so ChatThread's drain-effect stamp check (sessionId must
    // equal activeSessionId) sees a match instead of false-discarding it.
    const session = streamSessionManager.start("Arty");
    useChatStore.setState((draft: any) => {
      draft.agents.Arty.pendingMessage = { content: "queued before session id", sessionId: null, agent: "Arty" };
    });
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const state = useChatStore.getState().agents.Arty;
    expect(state.activeSessionId).toBe("s1");
    expect(state.pendingMessage?.sessionId).toBe("s1");
  });

  it("Fix H: does NOT touch a pendingMessage already stamped with a concrete sessionId (genuine later switch)", async () => {
    // A message queued while resumed into an EXISTING session (S0) must keep
    // its S0 stamp even if this stream's data-session-id reports a different
    // session — that's a real context mismatch the drain effect must still
    // catch, not a same-turn assignment to sync.
    const session = streamSessionManager.start("Arty");
    useChatStore.setState((draft: any) => {
      draft.agents.Arty.pendingMessage = { content: "queued for S0", sessionId: "S0", agent: "Arty" };
    });
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const state = useChatStore.getState().agents.Arty;
    expect(state.activeSessionId).toBe("s1");
    expect(state.pendingMessage?.sessionId).toBe("S0");
  });

  it("finish event leaves the finished assistant message with status \"complete\" (caret must not render)", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "t1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", delta: "hi", id: "t1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end", id: "t1" })}\n\n`,
      `data: ${JSON.stringify({ type: "finish" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const state = useChatStore.getState().agents.Arty;
    // messageSource may have already handed off to "finishing"/"history" by the
    // time the stream settles — read whichever mode still carries the message.
    const msgs =
      state.messageSource.mode === "live" || state.messageSource.mode === "finishing"
        ? state.messageSource.messages
        : [];
    const assistantMsg = msgs.find((m) => m.role === "assistant");
    expect(assistantMsg).toBeDefined();
    expect(assistantMsg?.status).toBe("complete");
    expect(assistantMsg?.status).not.toBe("streaming");
  });

  it("sync status \"running\" yields live message status \"streaming\"", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "sync", content: "partial", status: "running", error: null })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const assistantMsg = msgs.find((m) => m.role === "assistant");
    expect(assistantMsg?.status).toBe("streaming");
  });

  it("sync status \"finished\" yields live message status \"complete\"", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "sync", content: "done", status: "finished", error: null })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const assistantMsg = msgs.find((m) => m.role === "assistant");
    expect(assistantMsg?.status).toBe("complete");
  });

  it("sync status \"interrupted\" yields live message status \"complete\" (no blinking caret on a dead turn)", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "sync", content: "cut off", status: "interrupted", error: "restarted mid-stream" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const assistantMsg = msgs.find((m) => m.role === "assistant");
    expect(assistantMsg?.status).toBe("complete");
  });

  it("sync status \"error\" yields live message status \"complete\"", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "sync", content: "oops", status: "error", error: "fail" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const assistantMsg = msgs.find((m) => m.role === "assistant");
    expect(assistantMsg?.status).toBe("complete");
  });

  it("error event settles the current live message status to \"complete\" (caret stops on error)", async () => {
    // Seed a pre-existing live message (as if earlier throttled commits had
    // already landed it in the store) and a "start" frame carrying the SAME
    // id as messageId — this is how the real wire re-anchors
    // session.buffer.assistantId after processSSEStream's leading
    // buffer.reset() regenerates it to a fresh random uuid.
    const session = streamSessionManager.start("Arty");
    useChatStore.setState((draft: any) => {
      draft.agents.Arty.messageSource = {
        mode: "live",
        messages: [{
          id: "m-err",
          role: "assistant",
          parts: [{ type: "text", text: "partial" }],
          status: "streaming",
        }],
      };
    });
    const frames = [
      `data: ${JSON.stringify({ type: "start", messageId: "m-err" })}\n\n`,
      `data: ${JSON.stringify({ type: "error", errorText: "boom" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const assistantMsg = msgs.find((m) => m.id === "m-err");
    expect(assistantMsg?.status).toBe("complete");
  });

  it("BUG-C: connectionPhase stays error after finish when sync error preceded it", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "sync", content: "hi", status: "error", error: "fail" })}\n\n`,
      `data: ${JSON.stringify({ type: "finish" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    // finish after sync-error must NOT overwrite "error" with "streaming"
    expect(useChatStore.getState().agents.Arty.connectionPhase).toBe("error");
  });
});
