/**
 * T7 single connect path: sendMessage / interruptAndSend / resumeStream all
 * funnel through ONE connect point built on the T6 transport.
 *
 * - sendMessage POSTs the turn (startTurn) and then opens the GET envelope
 *   stream (openTurnStream) on the session id returned in the 202 body.
 * - The refresh/resume path (resumeStream action → renderer.connect) opens the
 *   SAME GET envelope stream, with no POST.
 *
 * This exercises the REAL store + REAL streaming-renderer with only the
 * `chat-stream` transport mocked, so `openTurnStream` is the observable proxy
 * for "connect was called with (agent, sessionId)".
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

// vi.hoisted so the mocked module factory can reference these before the
// top-level `const`s would otherwise be initialised.
const { startTurnMock, openTurnStreamMock } = vi.hoisted(() => ({
  startTurnMock: vi.fn(),
  openTurnStreamMock: vi.fn(),
}));

vi.mock("@/lib/query-client", () => ({
  queryClient: {
    invalidateQueries: vi.fn(),
    setQueryData: vi.fn(),
    getQueryData: vi.fn(() => undefined),
    getQueriesData: vi.fn(() => []),
    refetchQueries: vi.fn().mockResolvedValue(undefined),
  },
}));

vi.mock("@/stores/stream/chat-stream", () => ({
  startTurn: startTurnMock,
  openTurnStream: openTurnStreamMock,
}));

vi.mock("@/lib/api", () => ({
  apiGet: vi.fn().mockResolvedValue({}),
  apiPost: vi.fn().mockResolvedValue({}),
  apiPut: vi.fn().mockResolvedValue({}),
  apiPatch: vi.fn().mockResolvedValue({}),
  apiDelete: vi.fn().mockResolvedValue(undefined),
  getToken: () => "t",
  assertToken: () => "t",
  handleUnauthorized: vi.fn(),
}));

import { useChatStore } from "@/stores/chat-store";
import { emptyAgentState } from "@/stores/chat-types";

const AGENT = "main";
const flush = () => new Promise<void>((r) => setTimeout(r, 0));

beforeEach(() => {
  startTurnMock.mockReset();
  startTurnMock.mockResolvedValue({ session_id: "s1", user_message_id: "u1" });
  openTurnStreamMock.mockReset();
  useChatStore.setState({
    currentAgent: AGENT,
    agents: { [AGENT]: emptyAgentState() },
  });
});

describe("single connect path", () => {
  it("sendMessage posts then connects with returned session id", async () => {
    useChatStore.getState().sendMessage("hi");
    await flush();

    expect(startTurnMock).toHaveBeenCalledTimes(1);
    // connect(agent, sessionId) opens the GET envelope via openTurnStream — the
    // single connect point — using the session id from the 202 body.
    expect(openTurnStreamMock).toHaveBeenCalledWith(
      AGENT,
      "s1",
      expect.anything(),
      expect.anything(),
    );
  });

  it("refresh path uses the SAME connect", () => {
    useChatStore.getState().resumeStream(AGENT, "s1");

    // No POST on the resume path — it re-enters the identical connect point.
    expect(startTurnMock).not.toHaveBeenCalled();
    expect(openTurnStreamMock).toHaveBeenCalledWith(
      AGENT,
      "s1",
      expect.anything(),
      expect.anything(),
    );
  });

  it("regenerate posts then connects (202 path, not legacy POST-SSE)", async () => {
    // Seed a live overlay with a completed user→assistant exchange so
    // regenerate can derive the last user text.
    useChatStore.setState({
      currentAgent: AGENT,
      agents: {
        [AGENT]: {
          ...emptyAgentState(),
          activeSessionId: "s1",
          messageSource: {
            mode: "live",
            messages: [
              { id: "u1", role: "user", parts: [{ type: "text", text: "hi" }], createdAt: "", status: "complete" },
              { id: "a1", role: "assistant", parts: [{ type: "text", text: "yo" }], createdAt: "", status: "complete" },
            ],
          },
        },
      },
    });

    useChatStore.getState().regenerate();
    await flush();

    // Goes through the 202 turn-start (startTurn) then the single connect point,
    // NOT the deleted legacy POST-then-read-body-as-SSE transport.
    expect(startTurnMock).toHaveBeenCalledTimes(1);
    expect(openTurnStreamMock).toHaveBeenCalledWith(
      AGENT,
      "s1",
      expect.anything(),
      expect.anything(),
    );
  });
});
