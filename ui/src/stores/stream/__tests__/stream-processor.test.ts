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
    onStreamDone: vi.fn(),
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

  it("settles to idle (no reconnect) when a legacy stream ends without finish (T8)", async () => {
    // T8 removed the transport reconnect loop. The non-batchMode path now
    // settles a drop-without-finish to a non-active phase instead of scheduling
    // a reconnect.
    const session = streamSessionManager.start("Arty");
    const callbacks = makeCallbacks();
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", delta: "hi", id: "t1" })}\n\n`,
      // no finish event — stream closes
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks,
    });
    const phase = useChatStore.getState().agents.Arty.connectionPhase;
    expect(phase).not.toBe("streaming");
    expect(phase).not.toBe("submitted");
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
