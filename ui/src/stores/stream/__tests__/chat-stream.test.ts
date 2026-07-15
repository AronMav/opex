import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { openTurnStream, startTurn } from "../chat-stream";
import type { TurnStreamCallbacks } from "../chat-stream";
import { streamSessionManager } from "../../stream-session";
import { useChatStore } from "../../chat-store";
import { useAuthStore } from "../../auth-store";
import { getLiveMessages } from "../../chat-types";
import type { TextPart, ToolPart } from "../../chat-types";

// Build a ReadableStream from an array of already-formatted SSE frames
// ("data: {...}\n\n" strings) — same helper shape as
// stream-processor.test.ts's makeStream.
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

function frame(obj: Record<string, unknown>): string {
  return `data: ${JSON.stringify(obj)}\n\n`;
}

/** Wraps TurnStreamCallbacks with call-order tracking and a promise that
 *  resolves once the turn settles (onFinished or onConnectionLost — whichever
 *  fires first terminates openTurnStream's async work in this module). */
function trackCallbacks(overrides: Partial<TurnStreamCallbacks> = {}) {
  const order: string[] = [];
  let resolveSettled: () => void;
  const settled = new Promise<void>((resolve) => {
    resolveSettled = resolve;
  });
  const cb: TurnStreamCallbacks = {
    onEnvelopeApplied: () => {
      order.push("onEnvelopeApplied");
      overrides.onEnvelopeApplied?.();
    },
    onFinished: () => {
      order.push("onFinished");
      overrides.onFinished?.();
      resolveSettled();
    },
    onConnectionLost: () => {
      order.push("onConnectionLost");
      overrides.onConnectionLost?.();
      resolveSettled();
    },
  };
  return { cb, order, settled };
}

/** Drains any dangling async work processSSEStream kicked off after firing
 *  onFinished/onConnectionLost (e.g. the finishing→history refetch dance) so
 *  it can't bleed into the next test — mirrors the setTimeout-flush pattern
 *  used by the existing sse-stream.test.ts store-integration suite. */
function flush(ms = 20): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function liveText(agent: string): string {
  const msgs = getLiveMessages(useChatStore.getState().agents[agent].messageSource);
  return msgs
    .flatMap((m) => m.parts)
    .filter((p): p is TextPart => p.type === "text")
    .map((p) => p.text)
    .join("");
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
  useAuthStore.setState({ token: "test-token" });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("startTurn", () => {
  it("POSTs to /api/chat and returns the 202 body", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ session_id: "s1", user_message_id: "u1" }), { status: 202 }),
    );
    const result = await startTurn("Arty", { messages: [{ role: "user", content: "hi" }] });
    expect(result).toEqual({ session_id: "s1", user_message_id: "u1" });
    expect(fetchSpy).toHaveBeenCalledWith(
      "/api/chat",
      expect.objectContaining({ method: "POST" }),
    );
  });
});

describe("openTurnStream", () => {
  it("applies envelope as a single batch and fires callbacks in order", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      frame({ type: "sync_begin", runStatus: "running", truncated: false }),
      frame({ type: "start", messageId: "m-1" }),
      frame({ type: "text-start", id: "t1" }),
      frame({ type: "text-delta", delta: "Привет" }),
      frame({ type: "sync_end", lastSeq: 3 }),
    ];
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(makeStream(frames), { status: 200 }));

    let textAtEnvelopeApplied: string | null = null;
    const { cb, order, settled } = trackCallbacks({
      onEnvelopeApplied: () => {
        // Batch gate: the whole envelope commits at sync_end, so by the time
        // onEnvelopeApplied fires the accumulated text is present.
        textAtEnvelopeApplied = liveText("Arty");
      },
    });

    openTurnStream("Arty", "s1", session, cb);
    await settled;
    await flush();

    expect(textAtEnvelopeApplied).toBe("Привет");
    // runStatus was "running" and no `finish` event arrived before the fake
    // stream closed — the connection dropped without a terminal signal.
    expect(order).toContain("onConnectionLost");
    expect(order).not.toContain("onFinished");
  });

  it("empty finished envelope fires onFinished without touching live state", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      frame({ type: "sync_begin", runStatus: "finished", truncated: false }),
      frame({ type: "sync_end", lastSeq: null }),
    ];
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(makeStream(frames), { status: 200 }));

    const { cb, order, settled } = trackCallbacks();

    openTurnStream("Arty", "s1", session, cb);
    await settled;
    await flush();

    expect(order).toContain("onFinished");
    expect(order).not.toContain("onConnectionLost");
    expect(liveText("Arty")).toBe("");
  });

  it("network error before finish fires onConnectionLost", async () => {
    const session = streamSessionManager.start("Arty");
    vi.spyOn(globalThis, "fetch").mockRejectedValue(new Error("network down"));

    const { cb, order, settled } = trackCallbacks();
    openTurnStream("Arty", "s1", session, cb);
    await settled;
    await flush();

    expect(order).toEqual(["onConnectionLost"]);
  });

  it("envelope with tool events rebuilds tool parts in one batch (idempotent on re-apply)", async () => {
    const buildFrames = () => [
      frame({ type: "sync_begin", runStatus: "running", truncated: false }),
      frame({ type: "start", messageId: "m-1" }),
      frame({ type: "tool-input-start", toolCallId: "tc1", toolName: "search_web" }),
      frame({ type: "tool-input-available", toolCallId: "tc1", input: { query: "q" } }),
      frame({ type: "tool-output-available", toolCallId: "tc1", output: "ok" }),
      frame({ type: "sync_end", lastSeq: 4 }),
    ];

    function toolParts(): ToolPart[] {
      const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
      return msgs.flatMap((m) => m.parts).filter((p): p is ToolPart => p.type === "tool");
    }

    // First application.
    const session = streamSessionManager.start("Arty");
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(makeStream(buildFrames()), { status: 200 }));

    let commitsBeforeEnvelopeApplied = 0;
    const commitSpy = vi.spyOn(session, "commit");
    const { cb, settled } = trackCallbacks({
      onEnvelopeApplied: () => {
        commitsBeforeEnvelopeApplied = commitSpy.mock.calls.length;
      },
    });
    openTurnStream("Arty", "s1", session, cb);
    await settled;
    await flush();

    // Batch gate: no commit landed before sync_end's own commit() call.
    expect(commitsBeforeEnvelopeApplied).toBe(1);
    expect(toolParts()).toEqual([
      expect.objectContaining({ toolCallId: "tc1", state: "output-available", output: "ok" }),
    ]);

    // Re-apply the SAME envelope on the same (still-current) session — as if
    // reconnecting mid-turn and replaying identical events again.
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(makeStream(buildFrames()), { status: 200 }));
    const { cb: cb2, settled: settled2 } = trackCallbacks();
    openTurnStream("Arty", "s1", session, cb2);
    await settled2;
    await flush();

    const partsAfterReapply = toolParts();
    expect(partsAfterReapply).toEqual([
      expect.objectContaining({ toolCallId: "tc1", state: "output-available", output: "ok" }),
    ]);
    // Idempotent: still exactly one tool part, not duplicated.
    expect(partsAfterReapply.length).toBe(1);
  });
});
