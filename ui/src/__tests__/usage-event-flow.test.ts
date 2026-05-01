// ── usage-event-flow.test.ts ─────────────────────────────────────────────────
// Phase 2 todo #8 — coverage for the usage SSE event flow:
//   1. parseSseEvent (both layers: stores/sse-events.ts + stores/stream/sse-parser.ts)
//   2. processSSEStream `case "usage":` → AgentState writes
// PR #23 added these paths with zero tests; failures these tests would catch:
//   - Backend rename camelCase → snake_case
//   - Field swap (cacheReadTokens vs cacheCreationTokens) in stream-processor
//   - 0 vs absent distinction (some providers emit 0 for "no cache")

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { parseSseEvent as parseSseEventEvents } from "@/stores/sse-events";
import { parseSseEvent as parseSseEventStream } from "@/stores/stream/sse-parser";
import { useChatStore } from "@/stores/chat-store";

// Mock react-query (used inside chat-store for cache invalidation).
vi.mock("@/lib/query-client", () => ({
  queryClient: { invalidateQueries: vi.fn(), getQueryData: vi.fn(() => undefined) },
}));

// Mock api helpers — getToken reads localStorage which may not be set in jsdom.
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(),
  apiDelete: vi.fn(),
  apiPatch: vi.fn(),
  getToken: vi.fn(() => "test-token"),
  assertToken: vi.fn(() => "test-token"),
}));

// ── Helpers ──────────────────────────────────────────────────────────────────

/** Encode SSE events as a ReadableStream so chat-store's fetch picks them up. */
function makeSSEStream(events: object[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  const chunks = events.map((e) => encoder.encode(`data: ${JSON.stringify(e)}\n`));
  let i = 0;
  return new ReadableStream<Uint8Array>({
    pull(controller) {
      if (i < chunks.length) controller.enqueue(chunks[i++]);
      else controller.close();
    },
  });
}

function mockFetch(events: object[]) {
  return vi.spyOn(globalThis, "fetch").mockResolvedValue(
    new Response(makeSSEStream(events), { status: 200 }),
  );
}

// ── Parser tests (both layers) ───────────────────────────────────────────────

describe.each([
  ["stores/sse-events.ts", parseSseEventEvents],
  ["stores/stream/sse-parser.ts", parseSseEventStream],
])("parseSseEvent (%s) — usage event", (_label, parseSseEvent) => {
  it("parses full payload with all extended fields", () => {
    const e = parseSseEvent(
      JSON.stringify({
        type: "usage",
        inputTokens: 12500,
        outputTokens: 1800,
        cacheReadTokens: 8200,
        cacheCreationTokens: 1200,
        reasoningTokens: 600,
      }),
    );
    expect(e).toEqual({
      type: "usage",
      inputTokens: 12500,
      outputTokens: 1800,
      cacheReadTokens: 8200,
      cacheCreationTokens: 1200,
      reasoningTokens: 600,
    });
  });

  it("returns extended fields as undefined when SSE omits them", () => {
    const e = parseSseEvent(
      JSON.stringify({ type: "usage", inputTokens: 100, outputTokens: 50 }),
    );
    expect(e).toEqual({
      type: "usage",
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: undefined,
      cacheCreationTokens: undefined,
      reasoningTokens: undefined,
    });
  });

  it("preserves 0 vs absent distinction for cache fields", () => {
    const e = parseSseEvent(
      JSON.stringify({
        type: "usage",
        inputTokens: 100,
        outputTokens: 50,
        cacheCreationTokens: 0,
      }),
    );
    // Numeric zero must be preserved (not coerced to undefined / null).
    expect(e?.type === "usage" && e.cacheCreationTokens).toBe(0);
    // Untouched fields stay undefined.
    expect(e?.type === "usage" && e.cacheReadTokens).toBeUndefined();
    expect(e?.type === "usage" && e.reasoningTokens).toBeUndefined();
  });

  it("rejects non-numeric extended fields (defends against snake_case rename)", () => {
    // If the backend ever renames cacheReadTokens → cache_read_tokens, the
    // strongly-typed field will be undefined here. This pins the wire format.
    const e = parseSseEvent(
      JSON.stringify({
        type: "usage",
        inputTokens: 100,
        outputTokens: 50,
        cache_read_tokens: 7777, // wrong shape — should NOT populate cacheReadTokens
      }),
    );
    expect(e?.type === "usage" && e.cacheReadTokens).toBeUndefined();
  });

  it("defaults missing inputTokens/outputTokens to 0 (no NaN)", () => {
    const e = parseSseEvent(JSON.stringify({ type: "usage" }));
    expect(e).toEqual({
      type: "usage",
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: undefined,
      cacheCreationTokens: undefined,
      reasoningTokens: undefined,
    });
  });
});

// ── Stream-processor tests (drives chat-store.sendMessage) ──────────────────

describe("processSSEStream — case 'usage' writes AgentState fields", () => {
  const AGENT = "TestAgent";

  beforeEach(() => {
    useChatStore.setState({
      agents: {},
      currentAgent: AGENT,
      _selectCounter: {},
      sessionParticipants: {},
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("populates all 5 token fields when SSE emits a full usage payload", async () => {
    mockFetch([
      { type: "data-session-id", data: { sessionId: "sess-usage-1" } },
      { type: "start", messageId: "m1" },
      {
        type: "usage",
        inputTokens: 12500,
        outputTokens: 1800,
        cacheReadTokens: 8200,
        cacheCreationTokens: 1200,
        reasoningTokens: 600,
      },
      { type: "finish" },
    ]);

    useChatStore.getState().sendMessage("hi");
    await new Promise((r) => setTimeout(r, 200));

    const st = useChatStore.getState().agents[AGENT];
    expect(st?.contextTokens).toBe(12500);
    expect(st?.contextOutputTokens).toBe(1800);
    expect(st?.cacheReadTokens).toBe(8200);
    expect(st?.cacheCreationTokens).toBe(1200);
    expect(st?.reasoningTokens).toBe(600);
  });

  it("leaves extended fields null when SSE omits them", async () => {
    mockFetch([
      { type: "data-session-id", data: { sessionId: "sess-usage-2" } },
      { type: "start", messageId: "m1" },
      { type: "usage", inputTokens: 100, outputTokens: 50 },
      { type: "finish" },
    ]);

    useChatStore.getState().sendMessage("hi");
    await new Promise((r) => setTimeout(r, 200));

    const st = useChatStore.getState().agents[AGENT];
    expect(st?.contextTokens).toBe(100);
    expect(st?.contextOutputTokens).toBe(50);
    // Extended fields default to null (not 0) — ContextBar uses this to hide rows.
    expect(st?.cacheReadTokens).toBeNull();
    expect(st?.cacheCreationTokens).toBeNull();
    expect(st?.reasoningTokens).toBeNull();
  });

  it("preserves 0 (not null) when provider explicitly reports zero cache write", async () => {
    mockFetch([
      { type: "data-session-id", data: { sessionId: "sess-usage-3" } },
      { type: "start", messageId: "m1" },
      {
        type: "usage",
        inputTokens: 100,
        outputTokens: 50,
        cacheCreationTokens: 0,
      },
      { type: "finish" },
    ]);

    useChatStore.getState().sendMessage("hi");
    await new Promise((r) => setTimeout(r, 200));

    const st = useChatStore.getState().agents[AGENT];
    // ?? null only triggers on undefined/null. Numeric 0 must survive.
    expect(st?.cacheCreationTokens).toBe(0);
    // Other extended fields remain null.
    expect(st?.cacheReadTokens).toBeNull();
    expect(st?.reasoningTokens).toBeNull();
  });

  it("does not swap cacheReadTokens with cacheCreationTokens (regression guard)", async () => {
    // Distinct values catch a copy-paste bug in stream-processor where
    // event.cacheReadTokens might be assigned to cacheCreationTokens or vice versa.
    mockFetch([
      { type: "data-session-id", data: { sessionId: "sess-usage-4" } },
      { type: "start", messageId: "m1" },
      {
        type: "usage",
        inputTokens: 100,
        outputTokens: 50,
        cacheReadTokens: 1111,
        cacheCreationTokens: 9999,
      },
      { type: "finish" },
    ]);

    useChatStore.getState().sendMessage("hi");
    await new Promise((r) => setTimeout(r, 200));

    const st = useChatStore.getState().agents[AGENT];
    expect(st?.cacheReadTokens).toBe(1111);
    expect(st?.cacheCreationTokens).toBe(9999);
  });

  // ── Multi-agent isolation ────────────────────────────────────────────────
  // Documents a latent production bug (review L3, 2026-05-01): the `usage`
  // SSE event has no `agentName` field, so writeDraft applies it to the
  // currentAgent regardless of which agent actually emitted the usage. In
  // a multi-agent session, agent B finishing a stream while agent A is
  // mid-turn will overwrite A's tokenUsage with B's values.
  //
  // The fix requires (a) extending sse-events.UsageEvent with agentName,
  // (b) backend emitting the field, (c) stream-processor routing by agent.
  // Out of scope for PR #29 (test-only); tracked as a separate todo.

  it.todo(
    "Usage events are isolated per-agent in multi-agent sessions (FIXME: prod bug L3)",
  );

  it("overwrites earlier usage fields when a second usage event arrives", async () => {
    // Multi-turn loop: first turn reports cache-creation, second is a plain reuse.
    mockFetch([
      { type: "data-session-id", data: { sessionId: "sess-usage-5" } },
      { type: "start", messageId: "m1" },
      {
        type: "usage",
        inputTokens: 100,
        outputTokens: 50,
        cacheCreationTokens: 1200,
      },
      {
        type: "usage",
        inputTokens: 200,
        outputTokens: 80,
        cacheReadTokens: 1200,
      },
      { type: "finish" },
    ]);

    useChatStore.getState().sendMessage("hi");
    await new Promise((r) => setTimeout(r, 200));

    const st = useChatStore.getState().agents[AGENT];
    expect(st?.contextTokens).toBe(200);
    expect(st?.contextOutputTokens).toBe(80);
    expect(st?.cacheReadTokens).toBe(1200);
    // Second event omitted cacheCreationTokens → should reset to null, not stick at 1200.
    expect(st?.cacheCreationTokens).toBeNull();
  });
});
