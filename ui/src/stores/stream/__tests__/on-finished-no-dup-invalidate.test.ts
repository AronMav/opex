import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../chat-stream", () => ({
  startTurn: vi.fn(),
  openTurnStream: vi.fn(),
}));
import { openTurnStream } from "../chat-stream";
import { queryClient } from "@/lib/query-client";
import { createStreamingRenderer } from "../../streaming-renderer";
import { useChatStore } from "../../chat-store";

describe("onFinished does not duplicate post-finally invalidations (B1.4)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("connect().onFinished only idles the phase", () => {
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");
    const renderer = createStreamingRenderer({
      get: () => useChatStore.getState(),
      set: (fn) => useChatStore.setState((s) => { fn(s as never); }),
    });
    renderer.connect("A", "sid-1");
    const cb = vi.mocked(openTurnStream).mock.calls[0][3];
    invalidateSpy.mockClear();
    cb.onFinished();
    expect(invalidateSpy).not.toHaveBeenCalled();
    expect(useChatStore.getState().agents["A"]?.connectionPhase).toBe("idle");
  });
});
