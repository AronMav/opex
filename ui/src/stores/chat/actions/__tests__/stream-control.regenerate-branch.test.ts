/**
 * R2 (CRITICAL B + C + F): regenerate / regenerateFrom / forkAndRegenerate must
 * create a REAL branch (POST /fork), not append a forward child via /api/chat.
 *
 * These assert the shared forkAndStream flow:
 *  (a) POST /api/sessions/{id}/fork with the correct branch_from_message_id,
 *  (b) selectedBranches[parent] = resp.message_id,
 *  (c) invalidateQueries(sessionMessages) BEFORE sendTurn (F),
 *  (d) sendTurn called with resp.message_id (not a random uuid),
 *  (e) abortLocalOnly (NOT abortActiveStream) when a stream is active (C).
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

const { renderer, invalidateQueriesMock } = vi.hoisted(() => ({
  renderer: {
    sendTurn: vi.fn(),
    connect: vi.fn(),
    resumeStream: vi.fn(),
    abortActiveStream: vi.fn(),
    abortLocalOnly: vi.fn(),
    cleanupAgent: vi.fn(),
    getAbortCtrl: vi.fn(),
    setAbortCtrl: vi.fn(),
    getReconnectTimer: vi.fn(),
    setReconnectTimer: vi.fn(),
    onSessionId: vi.fn(),
  },
  invalidateQueriesMock: vi.fn(),
}));

vi.mock("@/lib/query-client", () => ({
  queryClient: {
    invalidateQueries: invalidateQueriesMock,
    setQueryData: vi.fn(),
    getQueryData: vi.fn(() => undefined),
    getQueriesData: vi.fn(() => []),
    refetchQueries: vi.fn().mockResolvedValue(undefined),
  },
}));

vi.mock("@/stores/streaming-renderer", () => ({
  createStreamingRenderer: () => renderer,
}));

const { apiPostMock } = vi.hoisted(() => ({ apiPostMock: vi.fn() }));

vi.mock("@/lib/api", () => ({
  apiGet: vi.fn().mockResolvedValue({}),
  apiPost: apiPostMock,
  apiPut: vi.fn().mockResolvedValue({}),
  apiPatch: vi.fn().mockResolvedValue({}),
  apiDelete: vi.fn().mockResolvedValue(undefined),
  getToken: () => "t",
  assertToken: () => "t",
  handleUnauthorized: vi.fn(),
}));

import { useChatStore } from "@/stores/chat-store";
import { emptyAgentState } from "@/stores/chat-types";
import type { ChatMessage } from "@/stores/chat-types";

const AGENT = "main";
const flush = () => new Promise<void>((r) => setTimeout(r, 0));

const U1: ChatMessage = { id: "u1", role: "user", parts: [{ type: "text", text: "hi" }], createdAt: "" };
const A1: ChatMessage = { id: "a1", role: "assistant", parts: [{ type: "text", text: "yo" }], createdAt: "" };

function seed(phase: "streaming" | "idle" = "streaming") {
  useChatStore.setState({
    currentAgent: AGENT,
    agents: {
      [AGENT]: {
        ...emptyAgentState(),
        activeSessionId: "s1",
        connectionPhase: phase,
        messageSource: { mode: "live", messages: [U1, A1] },
      },
    },
  });
}

beforeEach(() => {
  for (const fn of Object.values(renderer)) (fn as ReturnType<typeof vi.fn>).mockReset();
  invalidateQueriesMock.mockReset();
  apiPostMock.mockReset();
  apiPostMock.mockResolvedValue({
    message_id: "b1",
    parent_message_id: "p1",
    branch_from_message_id: "u1",
  });
});

describe("regenerate — real branch (B/C/F)", () => {
  it("POSTs /fork with the last user message id, selects the branch, invalidates before sendTurn, streams resp.message_id", async () => {
    seed();
    useChatStore.getState().regenerate();
    await flush();

    // (a) POST /fork with correct branch_from + content
    expect(apiPostMock).toHaveBeenCalledWith(
      "/api/sessions/s1/fork?agent=main",
      { branch_from_message_id: "u1", content: "hi" },
    );

    // (b) selectedBranches updated
    expect(useChatStore.getState().agents[AGENT]?.selectedBranches).toEqual({ p1: "b1" });

    // (c) invalidate sessionMessages BEFORE sendTurn (F)
    expect(invalidateQueriesMock).toHaveBeenCalledWith({ queryKey: ["sessions", "s1", "messages"] });
    const invOrder = invalidateQueriesMock.mock.invocationCallOrder[0];
    const sendOrder = renderer.sendTurn.mock.invocationCallOrder[0];
    expect(invOrder).toBeLessThan(sendOrder);

    // (d) sendTurn called with resp.message_id (not a random uuid)
    expect(renderer.sendTurn).toHaveBeenCalledWith(AGENT, "s1", "hi", undefined, "b1");
  });

  it("uses abortLocalOnly (NOT abortActiveStream) when a stream is active (C)", async () => {
    seed("streaming");
    useChatStore.getState().regenerate();
    await flush();

    expect(renderer.abortLocalOnly).toHaveBeenCalledWith(AGENT);
    expect(renderer.abortActiveStream).not.toHaveBeenCalled();
  });

  it("does not abort when no stream is active", async () => {
    seed("idle");
    useChatStore.getState().regenerate();
    await flush();

    expect(renderer.abortLocalOnly).not.toHaveBeenCalled();
    expect(renderer.abortActiveStream).not.toHaveBeenCalled();
  });
});

describe("regenerateFrom — real branch", () => {
  it("branches from the given USER message", async () => {
    seed();
    useChatStore.getState().regenerateFrom("u1");
    await flush();

    expect(apiPostMock).toHaveBeenCalledWith(
      "/api/sessions/s1/fork?agent=main",
      { branch_from_message_id: "u1", content: "hi" },
    );
    expect(renderer.sendTurn).toHaveBeenCalledWith(AGENT, "s1", "hi", undefined, "b1");
  });

  it("branches from the nearest PRECEDING user message when target is an assistant message", async () => {
    seed();
    useChatStore.getState().regenerateFrom("a1");
    await flush();

    // a1 is assistant → anchor on u1 (preceding user), not a1.
    expect(apiPostMock).toHaveBeenCalledWith(
      "/api/sessions/s1/fork?agent=main",
      { branch_from_message_id: "u1", content: "hi" },
    );
  });

  it("falls back to regenerate when the message id is not found", async () => {
    seed();
    useChatStore.getState().regenerateFrom("does-not-exist");
    await flush();

    // Fallback resolves the last user message (u1).
    expect(apiPostMock).toHaveBeenCalledWith(
      "/api/sessions/s1/fork?agent=main",
      { branch_from_message_id: "u1", content: "hi" },
    );
  });
});

describe("forkAndRegenerate — shared flow with edited content", () => {
  it("branches from messageId with the new content", async () => {
    seed();
    await useChatStore.getState().forkAndRegenerate("u1", "edited text");

    expect(apiPostMock).toHaveBeenCalledWith(
      "/api/sessions/s1/fork?agent=main",
      { branch_from_message_id: "u1", content: "edited text" },
    );
    expect(renderer.sendTurn).toHaveBeenCalledWith(AGENT, "s1", "edited text", undefined, "b1");
  });
});
