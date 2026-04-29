import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { SseConnection } from "@/lib/sse-connection";
import type { SseConnectionCallbacks, SseConnectionCallbacksWithPhase, SseEvent } from "@/lib/sse-connection";

// ── Helpers ──────────────────────────────────────────────────────────────────

const encoder = new TextEncoder();

/**
 * Creates a ReadableStream that emits SSE-formatted chunks, one per event.
 * Each chunk is a complete "data: <json>\n" line.
 */
function createMockStream(chunks: string[]): ReadableStream<Uint8Array> {
  let i = 0;
  return new ReadableStream<Uint8Array>({
    pull(controller) {
      if (i < chunks.length) {
        controller.enqueue(encoder.encode(chunks[i++]));
      } else {
        controller.close();
      }
    },
  });
}

function sseChunk(event: object): string {
  return `data: ${JSON.stringify(event)}\n`;
}

function mockFetchOk(chunks: string[]): void {
  vi.spyOn(globalThis, "fetch").mockResolvedValue(
    new Response(createMockStream(chunks), { status: 200 }),
  );
}

function makeCallbacks(): SseConnectionCallbacks & {
  events: SseEvent[];
  errors: string[];
  doneCalled: number;
} {
  const events: SseEvent[] = [];
  const errors: string[] = [];
  let doneCalled = 0;
  return {
    events,
    errors,
    get doneCalled() { return doneCalled; },
    onEvent: (e) => events.push(e),
    onError: (msg) => errors.push(msg),
    onDone: () => doneCalled++,
  };
}

function makeCallbacksWithPhase(): SseConnectionCallbacksWithPhase & {
  events: any[];
  errors: string[];
  doneCalled: number;
  phases: string[];
} {
  const events: any[] = [];
  const errors: string[] = [];
  const phases: string[] = [];
  let doneCalled = 0;
  return {
    events,
    errors,
    phases,
    get doneCalled() { return doneCalled; },
    onEvent: (e: any) => events.push(e),
    onError: (msg: string) => errors.push(msg),
    onDone: () => doneCalled++,
    onPhaseChange: (phase: string) => phases.push(phase),
  };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe("SseConnection — constructor and config", () => {
  afterEach(() => vi.restoreAllMocks());

  it("is initially active (not stopped)", () => {
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      { onEvent: vi.fn(), onError: vi.fn(), onDone: vi.fn() },
    );
    expect(conn.isActive).toBe(true);
  });
});

describe("SseConnection.connect() — POST new stream", () => {
  afterEach(() => vi.restoreAllMocks());

  it("calls fetch with correct URL, method, and Authorization header", async () => {
    mockFetchOk([sseChunk({ type: "finish" })]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: { agent: "Alice" }, token: "secret" },
      { onEvent: vi.fn(), onError: vi.fn(), onDone: vi.fn() },
    );
    await conn.connect();
    const [url, init] = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/chat");
    expect(init.method).toBe("POST");
    expect((init.headers as Record<string, string>)["Authorization"]).toBe("Bearer secret");
  });

  it("sends body as JSON for POST requests", async () => {
    mockFetchOk([sseChunk({ type: "finish" })]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: { agent: "Alice", messages: [] }, token: "tok" },
      { onEvent: vi.fn(), onError: vi.fn(), onDone: vi.fn() },
    );
    await conn.connect();
    const [, init] = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0] as [string, RequestInit];
    expect(init.body).toBe(JSON.stringify({ agent: "Alice", messages: [] }));
    expect((init.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
  });

  it("dispatches parsed SSE events to onEvent callback in order", async () => {
    const cbs = makeCallbacks();
    mockFetchOk([
      sseChunk({ type: "start", messageId: "m1" }),
      sseChunk({ type: "text-delta", delta: "hello" }),
      sseChunk({ type: "text-end" }),
      sseChunk({ type: "finish" }),
    ]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    await conn.connect();

    expect(cbs.events.length).toBe(4);
    expect(cbs.events[0].type).toBe("start");
    expect(cbs.events[1].type).toBe("text-delta");
    expect(cbs.events[2].type).toBe("text-end");
    expect(cbs.events[3].type).toBe("finish");
  });

  it("dispatches multiple rapid text-delta events without loss", async () => {
    const cbs = makeCallbacks();
    const deltas = Array.from({ length: 20 }, (_, i) => sseChunk({ type: "text-delta", delta: `d${i}` }));
    mockFetchOk(deltas);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    await conn.connect();
    const deltaEvents = cbs.events.filter(e => e.type === "text-delta");
    expect(deltaEvents.length).toBe(20);
    expect(cbs.doneCalled).toBe(1);
  });

  it("calls onDone when stream finishes naturally", async () => {
    const cbs = makeCallbacks();
    mockFetchOk([sseChunk({ type: "finish" })]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    await conn.connect();
    expect(cbs.doneCalled).toBe(1);
    expect(cbs.errors.length).toBe(0);
  });

  it("calls onError with error text on non-ok HTTP response", async () => {
    const cbs = makeCallbacks();
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response("Unauthorized", { status: 401 }),
    );
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "bad" },
      cbs,
    );
    await conn.connect();
    expect(cbs.errors.length).toBe(1);
    expect(cbs.errors[0]).toContain("Unauthorized");
    expect(cbs.doneCalled).toBe(0);
  });

  it("calls onError with HTTP status message when response body is empty", async () => {
    const cbs = makeCallbacks();
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response("", { status: 500 }),
    );
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    await conn.connect();
    expect(cbs.errors.length).toBe(1);
    expect(cbs.errors[0]).toContain("500");
  });

  it("skips [DONE] sentinel without calling onError", async () => {
    const cbs = makeCallbacks();
    mockFetchOk([
      sseChunk({ type: "text-delta", delta: "hi" }),
      "data: [DONE]\n",
    ]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    await conn.connect();
    expect(cbs.errors.length).toBe(0);
    expect(cbs.events[0].type).toBe("text-delta");
  });

  it("ignores non-data lines and malformed events", async () => {
    const cbs = makeCallbacks();
    mockFetchOk([
      "event: ping\n",
      ": heartbeat\n",
      "data: not-json\n",
      sseChunk({ type: "finish" }),
    ]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    await conn.connect();
    expect(cbs.errors.length).toBe(0);
    expect(cbs.events.length).toBe(1);
    expect(cbs.events[0].type).toBe("finish");
  });
});

describe("SseConnection.connect() — GET resume stream", () => {
  afterEach(() => vi.restoreAllMocks());

  it("calls GET /api/chat/{sessionId}/stream without a body", async () => {
    mockFetchOk([sseChunk({ type: "finish" })]);
    const conn = new SseConnection(
      { url: "/api/chat/sess-123/stream", method: "GET", token: "tok" },
      { onEvent: vi.fn(), onError: vi.fn(), onDone: vi.fn() },
    );
    await conn.connect();
    const [url, init] = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/chat/sess-123/stream");
    expect(init.method).toBe("GET");
    expect(init.body).toBeUndefined();
  });

  it("calls onDone (not onError) on 204 response", async () => {
    const cbs = makeCallbacks();
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(null, { status: 204 }),
    );
    const conn = new SseConnection(
      { url: "/api/chat/sess-abc/stream", method: "GET", token: "tok" },
      cbs,
    );
    await conn.connect();
    expect(cbs.doneCalled).toBe(1);
    expect(cbs.errors.length).toBe(0);
    expect(cbs.events.length).toBe(0);
  });
});

describe("SseConnection — onPhaseChange callbacks", () => {
  afterEach(() => vi.restoreAllMocks());

  it("calls onPhaseChange('connecting') at start and onPhaseChange('streaming') on first byte", async () => {
    const cbs = makeCallbacksWithPhase();
    mockFetchOk([sseChunk({ type: "finish" })]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    await conn.connect();
    expect(cbs.phases).toContain("connecting");
    expect(cbs.phases).toContain("streaming");
    const connectingIdx = cbs.phases.indexOf("connecting");
    const streamingIdx = cbs.phases.indexOf("streaming");
    expect(connectingIdx).toBeLessThan(streamingIdx);
  });

  it("calls onPhaseChange('done') when stream ends naturally", async () => {
    const cbs = makeCallbacksWithPhase();
    mockFetchOk([sseChunk({ type: "finish" })]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    await conn.connect();
    expect(cbs.phases).toContain("done");
  });
});

describe("SseConnection — reconnect lifecycle", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("calls onPhaseChange('reconnecting') when stream ends without finish event and sessionId is set", async () => {
    const cbs = makeCallbacksWithPhase();
    // Stream that ends without a finish event
    mockFetchOk([sseChunk({ type: "text-delta", delta: "partial" })]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-123");
    // Run connect but don't advance timers yet — reconnect is pending
    const connectPromise = conn.connect();
    await connectPromise;
    expect(cbs.phases).toContain("reconnecting");
  });

  it("retries with exponential backoff delays (1s, 2s, 4s)", async () => {
    const cbs = makeCallbacksWithPhase();
    let callCount = 0;
    vi.spyOn(globalThis, "fetch").mockImplementation(() => {
      callCount++;
      if (callCount === 1) {
        // Initial POST: stream that drops without finish
        return Promise.resolve(
          new Response(createMockStream([sseChunk({ type: "text-delta", delta: "hi" })]), { status: 200 }),
        );
      }
      // Retry GETs: return 500 to exhaust retries
      return Promise.resolve(new Response("error", { status: 500 }));
    });

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-abc");
    const connectPromise = conn.connect();
    await connectPromise;
    // First reconnect should be scheduled at 1s
    expect(callCount).toBe(1); // only initial fetch so far
    await vi.advanceTimersByTimeAsync(1000);
    expect(callCount).toBe(2); // first retry after 1s
    await vi.advanceTimersByTimeAsync(2000);
    expect(callCount).toBe(3); // second retry after 2s
    await vi.advanceTimersByTimeAsync(4000);
    expect(callCount).toBe(4); // third retry after 4s
  });

  it("calls onError('Max reconnect attempts exceeded') after max retries and onPhaseChange('error')", async () => {
    const cbs = makeCallbacksWithPhase();
    let callCount = 0;
    vi.spyOn(globalThis, "fetch").mockImplementation(() => {
      callCount++;
      if (callCount === 1) {
        return Promise.resolve(
          new Response(createMockStream([sseChunk({ type: "text-delta", delta: "hi" })]), { status: 200 }),
        );
      }
      return Promise.resolve(new Response("error", { status: 500 }));
    });

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-abc");
    const connectPromise = conn.connect();
    await connectPromise;
    // Advance through all retry delays: 1s + 2s + 4s
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(2000);
    await vi.advanceTimersByTimeAsync(4000);
    expect(cbs.errors).toContain("Max reconnect attempts exceeded");
    expect(cbs.phases).toContain("error");
  });

  it("stop() during reconnect backoff does NOT trigger further retries", async () => {
    const cbs = makeCallbacksWithPhase();
    let callCount = 0;
    vi.spyOn(globalThis, "fetch").mockImplementation(() => {
      callCount++;
      // Initial fetch: stream drops without finish
      return Promise.resolve(
        new Response(createMockStream([sseChunk({ type: "text-delta", delta: "hi" })]), { status: 200 }),
      );
    });

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-abc");
    const connectPromise = conn.connect();
    await connectPromise;
    // Reconnect should be pending now (phases contains "reconnecting")
    expect(cbs.phases).toContain("reconnecting");
    // Stop before the backoff timer fires
    conn.stop();
    // Advance 5 seconds — should not trigger any retry fetch
    await vi.advanceTimersByTimeAsync(5000);
    expect(callCount).toBe(1); // only the initial fetch
  });

  it("reconnect uses GET /api/chat/{sessionId}/stream (resume endpoint)", async () => {
    const cbs = makeCallbacksWithPhase();
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation((url, init) => {
      if ((init as RequestInit)?.method === "POST") {
        // Initial POST: stream drops without finish
        return Promise.resolve(
          new Response(createMockStream([sseChunk({ type: "text-delta", delta: "hi" })]), { status: 200 }),
        );
      }
      // GET resume: return 204 (engine finished)
      return Promise.resolve(new Response(null, { status: 204 }));
    });

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-xyz");
    const connectPromise = conn.connect();
    await connectPromise;
    await vi.advanceTimersByTimeAsync(1000);
    // Check that GET was called with the correct resume URL
    const calls = fetchMock.mock.calls;
    const getCall = calls.find(([, init]) => (init as RequestInit)?.method === "GET");
    expect(getCall).toBeDefined();
    expect(getCall![0]).toBe("/api/chat/sess-xyz/stream");
  });

  it("on 204 resume response, calls onDone (natural completion, not error)", async () => {
    const cbs = makeCallbacksWithPhase();
    vi.spyOn(globalThis, "fetch").mockImplementation((url, init) => {
      if ((init as RequestInit)?.method === "POST") {
        return Promise.resolve(
          new Response(createMockStream([sseChunk({ type: "text-delta", delta: "hi" })]), { status: 200 }),
        );
      }
      return Promise.resolve(new Response(null, { status: 204 }));
    });

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-xyz");
    const connectPromise = conn.connect();
    await connectPromise;
    await vi.advanceTimersByTimeAsync(1000);
    expect(cbs.doneCalled).toBe(1);
    expect(cbs.errors.length).toBe(0);
  });

  it("if resume returns non-ok status, counts as failed attempt and retries", async () => {
    const cbs = makeCallbacksWithPhase();
    let callCount = 0;
    vi.spyOn(globalThis, "fetch").mockImplementation((_url, init) => {
      callCount++;
      if ((init as RequestInit)?.method === "POST") {
        return Promise.resolve(
          new Response(createMockStream([sseChunk({ type: "text-delta", delta: "hi" })]), { status: 200 }),
        );
      }
      // First retry: 503, second: 204
      if (callCount === 2) return Promise.resolve(new Response("", { status: 503 }));
      return Promise.resolve(new Response(null, { status: 204 }));
    });

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-retry");
    const connectPromise = conn.connect();
    await connectPromise;
    await vi.advanceTimersByTimeAsync(1000); // first retry → 503
    await vi.advanceTimersByTimeAsync(2000); // second retry → 204
    expect(cbs.doneCalled).toBe(1);
  });

  it("does NOT reconnect when no sessionId is set (no session to resume)", async () => {
    const cbs = makeCallbacksWithPhase();
    mockFetchOk([sseChunk({ type: "text-delta", delta: "hi" })]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    // Do NOT call conn.setSessionId(...)
    const connectPromise = conn.connect();
    await connectPromise;
    await vi.advanceTimersByTimeAsync(5000);
    // No reconnect phases should be emitted
    expect(cbs.phases).not.toContain("reconnecting");
    // onDone called (treated as natural end since no session to reconnect)
    expect(cbs.doneCalled).toBe(1);
  });
});

describe("SseConnection — lastEventId tracking and Last-Event-ID header", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("tracks lastEventId from id: lines in the stream", async () => {
    const cbs = makeCallbacksWithPhase();
    // Stream with id: lines interleaved
    mockFetchOk([
      "id: 5\n",
      sseChunk({ type: "text-delta", delta: "hi" }),
      "id: 6\n",
      sseChunk({ type: "finish" }),
    ]);
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    await conn.connect();
    // Should have processed events normally
    expect(cbs.events.length).toBe(2);
    expect(cbs.doneCalled).toBe(1);
  });

  it("sends Last-Event-ID header on retryConnect when lastEventId is set", async () => {
    const cbs = makeCallbacksWithPhase();
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation((_url, init) => {
      if ((init as RequestInit)?.method === "POST") {
        // Initial POST: stream with id: line, then drops without finish
        return Promise.resolve(
          new Response(createMockStream([
            "id: 42\n",
            sseChunk({ type: "text-delta", delta: "hi" }),
          ]), { status: 200 }),
        );
      }
      // GET resume: return 204
      return Promise.resolve(new Response(null, { status: 204 }));
    });

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-eid");
    const connectPromise = conn.connect();
    await connectPromise;
    await vi.advanceTimersByTimeAsync(1000);

    // Check that GET retry included Last-Event-ID header
    const getCalls = fetchMock.mock.calls.filter(([, init]) => (init as RequestInit)?.method === "GET");
    expect(getCalls).toHaveLength(1);
    const getHeaders = (getCalls[0][1] as RequestInit).headers as Record<string, string>;
    expect(getHeaders["Last-Event-ID"]).toBe("42");
  });

  it("does NOT send Last-Event-ID header when lastEventId is null", async () => {
    const cbs = makeCallbacksWithPhase();
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation((_url, init) => {
      if ((init as RequestInit)?.method === "POST") {
        // Initial POST: stream drops without finish, no id: lines
        return Promise.resolve(
          new Response(createMockStream([
            sseChunk({ type: "text-delta", delta: "hi" }),
          ]), { status: 200 }),
        );
      }
      // GET resume: return 204
      return Promise.resolve(new Response(null, { status: 204 }));
    });

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-no-eid");
    const connectPromise = conn.connect();
    await connectPromise;
    await vi.advanceTimersByTimeAsync(1000);

    const getCalls = fetchMock.mock.calls.filter(([, init]) => (init as RequestInit)?.method === "GET");
    expect(getCalls).toHaveLength(1);
    const getHeaders = (getCalls[0][1] as RequestInit).headers as Record<string, string>;
    expect(getHeaders["Last-Event-ID"]).toBeUndefined();
  });

  it("handles 410 response by calling onDone (not onError)", async () => {
    const cbs = makeCallbacksWithPhase();
    vi.spyOn(globalThis, "fetch").mockImplementation((_url, init) => {
      if ((init as RequestInit)?.method === "POST") {
        // Initial POST: stream drops without finish
        return Promise.resolve(
          new Response(createMockStream([
            sseChunk({ type: "text-delta", delta: "hi" }),
          ]), { status: 200 }),
        );
      }
      // GET resume: return 410 (stream expired)
      return Promise.resolve(new Response("Gone", { status: 410 }));
    });

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok", maxRetries: 3 },
      cbs,
    );
    conn.setSessionId("sess-410");
    const connectPromise = conn.connect();
    await connectPromise;
    await vi.advanceTimersByTimeAsync(1000);

    expect(cbs.doneCalled).toBe(1);
    expect(cbs.errors.length).toBe(0);
    expect(cbs.phases).toContain("done");
  });
});

describe("SseConnection.stop()", () => {
  afterEach(() => vi.restoreAllMocks());

  it("sets isActive to false after stop()", () => {
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      { onEvent: vi.fn(), onError: vi.fn(), onDone: vi.fn() },
    );
    expect(conn.isActive).toBe(true);
    conn.stop();
    expect(conn.isActive).toBe(false);
  });

  it("aborts an in-flight fetch on stop()", async () => {
    vi.spyOn(globalThis, "fetch").mockImplementation(
      (_url: RequestInfo | URL, init?: RequestInit) =>
        new Promise<Response>((resolve) => {
          init?.signal?.addEventListener("abort", () => {
            resolve(new Response(null, { status: 200 }));
          });
        }),
    );

    const cbs = makeCallbacks();
    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      cbs,
    );
    const connectPromise = conn.connect();
    conn.stop();
    await connectPromise;
    // After abort, no onEvent calls from a stopped connection
    expect(cbs.events.length).toBe(0);
    expect(conn.isActive).toBe(false);
  });

  it("does not call onEvent after stop()", async () => {
    // Use a stream that is slow to produce chunks
    const onEvent = vi.fn();
    let streamController!: ReadableStreamDefaultController<Uint8Array>;
    const slowStream = new ReadableStream<Uint8Array>({
      start(controller) { streamController = controller; },
    });
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(slowStream, { status: 200 }));

    const conn = new SseConnection(
      { url: "/api/chat", method: "POST", body: {}, token: "tok" },
      { onEvent, onError: vi.fn(), onDone: vi.fn() },
    );

    const connectPromise = conn.connect();
    // Stop before any chunks arrive
    conn.stop();
    // Close the stream so the reader.read() loop can exit
    streamController.close();
    await connectPromise;

    expect(onEvent).not.toHaveBeenCalled();
    expect(conn.isActive).toBe(false);
  });
});
