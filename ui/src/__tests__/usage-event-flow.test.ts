// ── usage-event-flow.test.ts ─────────────────────────────────────────────────
// Phase 2 todo #8 — coverage for the usage SSE event flow:
//   1. parseSseEvent (single source: stores/sse-events.ts; the duplicate in
//      stores/stream/sse-parser.ts was deleted in S6.5 cleanup)
//   2. processSSEStream `case "usage":` → AgentState writes
// PR #23 added these paths with zero tests; failures these tests would catch:
//   - Backend rename camelCase → snake_case
//   - Field swap (cacheReadTokens vs cacheCreationTokens) in stream-processor
//   - 0 vs absent distinction (some providers emit 0 for "no cache")

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { parseSseEvent } from "@/stores/sse-events";
import { useChatStore } from "@/stores/chat-store";

// Mock react-query (used inside chat-store for cache invalidation).
// refetchQueries is required: the connect path's post-finally awaits it during
// the finishing→history handoff; without it the promise rejects and
// openTurnStream mis-reads the throw as a connection loss → infinite reconnect.
vi.mock("@/lib/query-client", () => ({
  queryClient: {
    invalidateQueries: vi.fn(),
    getQueryData: vi.fn(() => undefined),
    refetchQueries: vi.fn(() => Promise.resolve()),
  },
}));

// Mock api helpers — getToken reads localStorage which may not be set in jsdom.
// T7/T8: sendMessage POSTs via apiPost (startTurn) then opens the GET envelope
// stream served by the fetch spy below.
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(),
  apiDelete: vi.fn(),
  apiPatch: vi.fn(),
  apiPost: vi.fn().mockResolvedValue({ session_id: "sess-usage", user_message_id: "u1" }),
  getToken: vi.fn(() => "test-token"),
  assertToken: vi.fn(() => "test-token"),
  handleUnauthorized: vi.fn(),
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

// ── Parser tests ─────────────────────────────────────────────────────────────
// NOTE (S6.5): the parser is now a thin pass-through — codegen guarantees the
// shape from the Rust source of truth. Per-variant defaulting (e.g. inputTokens
// → 0 on absence) was removed; field-defaulting now happens in the
// stream-processor consumer (see "processSSEStream" tests below).

describe("parseSseEvent — usage event", () => {
  it("parses full payload with all extended fields", () => {
    const e = parseSseEvent(
      JSON.stringify({
        type: "usage",
        inputTokens: 12500,
        outputTokens: 1800,
        agentName: "TestAgent",
        cacheReadTokens: 8200,
        cacheCreationTokens: 1200,
        reasoningTokens: 600,
      }),
    );
    expect(e).toEqual({
      type: "usage",
      inputTokens: 12500,
      outputTokens: 1800,
      agentName: "TestAgent",
      cacheReadTokens: 8200,
      cacheCreationTokens: 1200,
      reasoningTokens: 600,
    });
  });

  it("preserves 0 vs absent distinction for cache fields", () => {
    const e = parseSseEvent(
      JSON.stringify({
        type: "usage",
        inputTokens: 100,
        outputTokens: 50,
        agentName: "TestAgent",
        cacheCreationTokens: 0,
      }),
    );
    // Numeric zero must be preserved (not coerced to undefined / null).
    expect(e?.type === "usage" && e.cacheCreationTokens).toBe(0);
    // Untouched fields are absent on the parsed object (not present in JSON).
    expect(e?.type === "usage" && (e as { cacheReadTokens?: number }).cacheReadTokens).toBeUndefined();
    expect(e?.type === "usage" && (e as { reasoningTokens?: number }).reasoningTokens).toBeUndefined();
  });
});

// ── Stream-processor tests (drives chat-store.sendMessage) ──────────────────

describe("processSSEStream — case 'usage' writes AgentState fields", () => {
  const AGENT = "TestAgent";

  beforeEach(() => {
    useChatStore.setState({
      agents: {},
      currentAgent: AGENT,
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

  // ── Multi-agent isolation (L3 fix verification) ──────────────────────────
  // Backend now tags every usage SSE event with `agentName` (chat.rs current
  // responding agent). Stream-processor routes the write to that target's
  // AgentState directly via setState — bypassing session.writeDraft which is
  // bound to the session's owner agent. Without this routing, usage from
  // peer agent B would corrupt session-owner A's tokenUsage state.

  it("routes usage by event.agentName, not by session-bound agent", async () => {
    // Both agents pre-exist in store (multi-agent session pattern). Stream
    // session is owned by AGENT (currentAgent), but a usage event tagged
    // for "Peer" must land in Peer's state, NOT in AGENT's.
    const { emptyAgentState } = await import("@/stores/chat-types");
    useChatStore.setState({
      agents: {
        [AGENT]: emptyAgentState(),
        Peer: emptyAgentState(),
      },
      currentAgent: AGENT,
      sessionParticipants: {},
    });

    mockFetch([
      { type: "data-session-id", data: { sessionId: "sess-multi-1" } },
      { type: "start", messageId: "m1", agentName: AGENT },
      // First usage — owner agent (AGENT). Should land on AGENT.
      {
        type: "usage",
        agentName: AGENT,
        inputTokens: 100,
        outputTokens: 50,
        cacheReadTokens: 1111,
      },
      // Second usage — peer agent. Must land on Peer, NOT overwrite AGENT.
      {
        type: "usage",
        agentName: "Peer",
        inputTokens: 9999,
        outputTokens: 8888,
        cacheReadTokens: 7777,
      },
      { type: "finish" },
    ]);

    useChatStore.getState().sendMessage("hi");
    await new Promise((r) => setTimeout(r, 200));

    const state = useChatStore.getState();
    // AGENT (session owner) preserves its own usage — NOT corrupted by Peer.
    expect(state.agents[AGENT]?.contextTokens).toBe(100);
    expect(state.agents[AGENT]?.contextOutputTokens).toBe(50);
    expect(state.agents[AGENT]?.cacheReadTokens).toBe(1111);
    // Peer received its own usage despite the stream being owned by AGENT.
    expect(state.agents.Peer?.contextTokens).toBe(9999);
    expect(state.agents.Peer?.contextOutputTokens).toBe(8888);
    expect(state.agents.Peer?.cacheReadTokens).toBe(7777);
  });

  it("falls back to session-bound agent when usage event omits agentName (legacy backend)", async () => {
    // Older backends not yet upgraded won't tag the event. Behavior must
    // gracefully degrade to single-agent mode (write to session.agent).
    mockFetch([
      { type: "data-session-id", data: { sessionId: "sess-legacy-1" } },
      { type: "start", messageId: "m1" },
      { type: "usage", inputTokens: 42, outputTokens: 7 },
      { type: "finish" },
    ]);

    useChatStore.getState().sendMessage("hi");
    await new Promise((r) => setTimeout(r, 200));

    expect(useChatStore.getState().agents[AGENT]?.contextTokens).toBe(42);
  });

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
