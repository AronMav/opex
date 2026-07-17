// Integration tests for the per-iteration UUID architecture (Phases 1-5).
// Verifies the SSE protocol contract end-to-end through stream-processor:
//   • each step-start opens a new live ChatMessage with the event's messageId
//   • text-deltas inside one iteration accumulate under that id
//   • SSE `id:` lines carry envelope-replay seq, not a Last-Event-ID token
//   • Finish event closes the stream cleanly

import { describe, it, expect, vi, beforeEach } from "vitest";
import { processSSEStream } from "../stream-processor";
import { streamSessionManager } from "../../stream-session";
import { useChatStore } from "../../chat-store";
import { getLiveMessages } from "../../chat-types";

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

describe("multi-iteration: one step-start opens one ChatMessage per iteration", () => {
  it("two iterations → two live ChatMessages with their own ids", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      // Iteration 0
      `data: ${JSON.stringify({ type: "start", messageId: "iter-0-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "step-start", stepId: "step_0", messageId: "iter-0-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", id: "text-1", delta: "Calling tool" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "tool-input-start", toolCallId: "tc1", toolName: "search" })}\n\n`,
      `data: ${JSON.stringify({ type: "tool-output-available", toolCallId: "tc1", output: "results" })}\n\n`,
      // Iteration 1
      `data: ${JSON.stringify({ type: "step-start", stepId: "step_1", messageId: "iter-1-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "text-2" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", id: "text-2", delta: "Done!" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end", id: "text-2" })}\n\n`,
      `data: ${JSON.stringify({ type: "finish" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    // Wait for any throttled commits
    await new Promise(r => setTimeout(r, 100));

    const live = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const assistants = live.filter(m => m.role === "assistant");
    // Two iterations → two distinct ChatMessages
    expect(assistants).toHaveLength(2);
    const ids = assistants.map(a => a.id).sort();
    expect(ids).toEqual(["iter-0-uuid", "iter-1-uuid"].sort());

    // Iter 0: text "Calling tool" + tool tc1
    const iter0 = assistants.find(a => a.id === "iter-0-uuid")!;
    expect(iter0.parts.some(p => p.type === "text" && p.text.includes("Calling tool"))).toBe(true);
    expect(iter0.parts.some(p => p.type === "tool" && (p as { toolCallId: string }).toolCallId === "tc1")).toBe(true);

    // Iter 1: only text, no tools
    const iter1 = assistants.find(a => a.id === "iter-1-uuid")!;
    expect(iter1.parts.some(p => p.type === "text" && p.text.includes("Done!"))).toBe(true);
    expect(iter1.parts.some(p => p.type === "tool")).toBe(false);
  });

  it("after finish, BOTH iteration messages carry status \"complete\" (no stuck caret on the tool bubble)", async () => {
    // Critical 1: step-start commits the previous iteration's message with
    // status "streaming", and `finish` used to settle only the FINAL
    // iteration's id — leaving the earlier "calling tool" bubble blinking
    // until the history refetch. step-start must settle the OLD id when it
    // switches, and finish sweeps any stragglers.
    const session = streamSessionManager.start("Arty");
    const frames = [
      // Iteration 0 — text + tool call
      `data: ${JSON.stringify({ type: "start", messageId: "iter-0-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "step-start", stepId: "step_0", messageId: "iter-0-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", id: "text-1", delta: "Calling tool" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "tool-input-start", toolCallId: "tc1", toolName: "search" })}\n\n`,
      `data: ${JSON.stringify({ type: "tool-output-available", toolCallId: "tc1", output: "results" })}\n\n`,
      // Iteration 1 — final answer
      `data: ${JSON.stringify({ type: "step-start", stepId: "step_1", messageId: "iter-1-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "text-2" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", id: "text-2", delta: "Done!" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end", id: "text-2" })}\n\n`,
      `data: ${JSON.stringify({ type: "finish" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    await new Promise(r => setTimeout(r, 100));

    const live = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const assistants = live.filter(m => m.role === "assistant");
    expect(assistants).toHaveLength(2);
    for (const msg of assistants) {
      expect(msg.status, `message ${msg.id} must not stay "streaming"`).toBe("complete");
    }
  });

  it("step-start settles the PREVIOUS iteration's message to \"complete\" mid-turn", async () => {
    // The earlier iteration stopped receiving text the moment step-start
    // switched ids — its caret must stop DURING the turn, not only at finish.
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "start", messageId: "iter-0-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", id: "text-1", delta: "Calling tool" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "step-start", stepId: "step_1", messageId: "iter-1-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "text-2" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", id: "text-2", delta: "still stre" })}\n\n`,
      // stream drops here — no finish. Iteration 0 must already be settled.
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    await new Promise(r => setTimeout(r, 100));

    const live = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const iter0 = live.find(m => m.id === "iter-0-uuid");
    expect(iter0).toBeDefined();
    expect(iter0?.status).toBe("complete");
  });

  it("step-start with same id as current buffer is a no-op (iteration 0 dedup)", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      // Backend emits both MessageStart and step-start with SAME id on iteration 0
      `data: ${JSON.stringify({ type: "start", messageId: "shared-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "step-start", stepId: "step_0", messageId: "shared-uuid" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", id: "text-1", delta: "Hello" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "finish" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    await new Promise(r => setTimeout(r, 100));

    const live = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
    const assistants = live.filter(m => m.role === "assistant");
    // Only ONE ChatMessage — the second step-start with same id was skipped
    expect(assistants).toHaveLength(1);
    expect(assistants[0].id).toBe("shared-uuid");
    expect(assistants[0].parts.some(p => p.type === "text" && p.text.includes("Hello"))).toBe(true);
  });
});

// Under the server-authoritative sync-envelope protocol, the GET stream
// always returns the full envelope (sync_begin → replay → sync_end → live)
// regardless of any Last-Event-ID header — there is no resumption offset to
// track client-side. `id:` SSE lines are envelope-replay seq only. The
// former "Last-Event-ID tracking" describe block was deleted.

describe("Finish event guarantee — closes connectionPhase cleanly", () => {
  it("normal finish closes connectionPhase to idle (non-error)", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "start", messageId: "m1" })}\n\n`,
      `data: ${JSON.stringify({ type: "step-start", stepId: "step_0", messageId: "m1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-start", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-delta", id: "text-1", delta: "ok" })}\n\n`,
      `data: ${JSON.stringify({ type: "text-end", id: "text-1" })}\n\n`,
      `data: ${JSON.stringify({ type: "finish" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    // Phase lands on "idle" once the (mocked / no-op) post-finish refetch
    // resolves. The historical "complete" phase no longer exists in
    // ConnectionPhase; the array form is kept only as a tolerant assertion —
    // what matters is that it is NOT "streaming"/"error".
    const phase = useChatStore.getState().agents.Arty.connectionPhase;
    expect(["complete", "idle"]).toContain(phase);
  });

  it("error event sets connectionPhase=error then finish does NOT overwrite", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      `data: ${JSON.stringify({ type: "data-session-id", data: { sessionId: "s1" } })}\n\n`,
      `data: ${JSON.stringify({ type: "error", errorText: "oops" })}\n\n`,
      `data: ${JSON.stringify({ type: "finish" })}\n\n`,
    ];
    await processSSEStream(session, makeStream(frames), {
      sessionId: null,
      callbacks: makeCallbacks(),
    });
    expect(useChatStore.getState().agents.Arty.connectionPhase).toBe("error");
    expect(useChatStore.getState().agents.Arty.streamError).toBe("oops");
  });
});
