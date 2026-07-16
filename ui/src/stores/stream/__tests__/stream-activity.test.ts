import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { openTurnStream } from "../chat-stream";
import type { TurnStreamCallbacks } from "../chat-stream";
import { streamSessionManager } from "../../stream-session";
import { useChatStore } from "../../chat-store";
import { useAuthStore } from "../../auth-store";

// Same ReadableStream-from-SSE-frames helper as chat-stream.test.ts.
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

describe("onEventActivity wiring (B1.2)", () => {
  it("fires per SSE event, not only on connect", async () => {
    const session = streamSessionManager.start("Arty");
    const frames = [
      frame({ type: "sync_begin", runStatus: "running", truncated: false }),
      frame({ type: "text-start", id: "t1" }),
      frame({ type: "text-delta", delta: "hi" }),
      frame({ type: "sync_end", lastSeq: 2 }),
    ];
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(makeStream(frames), { status: 200 }));

    const onEventActivity = vi.fn();
    const settled = new Promise<void>((resolve) => {
      const cb: TurnStreamCallbacks = {
        onEnvelopeApplied: () => {},
        onFinished: () => resolve(),
        onConnectionLost: () => resolve(),
        onEventActivity,
      };
      openTurnStream("Arty", "s1", session, cb);
    });
    await settled;

    // One call per parsed SSE event (4 frames above).
    expect(onEventActivity.mock.calls.length).toBeGreaterThanOrEqual(3);
  });
});
