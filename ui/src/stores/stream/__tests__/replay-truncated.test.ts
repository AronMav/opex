import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { openTurnStream } from "../chat-stream";
import type { TurnStreamCallbacks } from "../chat-stream";
import { streamSessionManager } from "../../stream-session";
import { useChatStore } from "../../chat-store";
import { useAuthStore } from "../../auth-store";

// Same ReadableStream-from-SSE-frames helper as chat-stream.test.ts /
// stream-activity.test.ts.
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

function resetAgentA(): void {
  useChatStore.setState((draft: any) => {
    draft.agents = {
      A: {
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
        replayTruncated: false,
      },
    };
  });
  streamSessionManager.disposeCurrent("A");
}

beforeEach(() => {
  resetAgentA();
  useAuthStore.setState({ token: "test-token" });
});

afterEach(() => {
  vi.restoreAllMocks();
});

async function runEnvelope(frames: string[]): Promise<void> {
  const session = streamSessionManager.start("A");
  vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(makeStream(frames), { status: 200 }));

  await new Promise<void>((resolve) => {
    const cb: TurnStreamCallbacks = {
      onEnvelopeApplied: () => {},
      onFinished: () => resolve(),
      onConnectionLost: () => resolve(),
    };
    openTurnStream("A", "s1", session, cb);
  });
}

describe("sync_begin.truncated -> AgentState.replayTruncated", () => {
  it("sets replayTruncated=true when the replay envelope was truncated", async () => {
    await runEnvelope([
      frame({ type: "sync_begin", runStatus: "running", truncated: true }),
      frame({ type: "sync_end", lastSeq: 1 }),
    ]);

    expect(useChatStore.getState().agents["A"]?.replayTruncated).toBe(true);
  });

  it("leaves replayTruncated=false when the replay envelope was not truncated", async () => {
    await runEnvelope([
      frame({ type: "sync_begin", runStatus: "running", truncated: false }),
      frame({ type: "sync_end", lastSeq: 1 }),
    ]);

    expect(useChatStore.getState().agents["A"]?.replayTruncated).toBe(false);
  });
});
