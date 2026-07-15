// ui/src/stores/stream/__tests__/streaming-integration.test.ts
import { describe, it, expect, beforeEach } from "vitest";
import { createFixtureStream, countDataLines } from "./sse-fixture-replay";
import { useChatStore } from "@/stores/chat-store";

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
});

describe("streaming integration — short response fixture", () => {
  it("fixture file exists and has content", () => {
    expect(countDataLines("short-response.sse")).toBeGreaterThan(0);
  });

  it("emits chunks progressively via the harness", async () => {
    const stream = createFixtureStream("short-response.sse", { chunkBytes: 128, delayMs: 0 });
    const reader = stream.getReader();
    let totalBytes = 0;
    let chunkCount = 0;
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      totalBytes += value.byteLength;
      chunkCount++;
    }
    expect(totalBytes).toBeGreaterThan(0);
    expect(chunkCount).toBeGreaterThan(0);
  });

  // Note: full end-to-end tests that pipe the fixture stream THROUGH
  // the actual processSSEStream are added progressively in Phase 3
  // (Tasks 3.4, 4.3). This baseline task only locks in the harness
  // and the fixture existence contract. The richer assertions depend
  // on either the current `processSSEStream` (pre-refactor) or the
  // extracted `stream-processor.ts` (post-refactor) — the test file
  // structure is extended in-place as phases complete.
});
