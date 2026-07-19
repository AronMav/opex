// ui/src/stores/__tests__/stream-session.test.ts
import { describe, it, expect, vi, beforeEach } from "vitest";
import { StreamSession, streamSessionManager } from "../stream-session";
import { useChatStore } from "../chat-store";
import { getLiveMessages, STREAM_THROTTLE_MS } from "../chat-types";

beforeEach(() => {
  // Reset the store to a known state with one agent.
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
      },
    };
  });
  streamSessionManager.disposeCurrent("Arty");
});

describe("StreamSession", () => {
  it("write applies when session is current", () => {
    const s = streamSessionManager.start("Arty");
    s.write({ connectionPhase: "streaming" });
    expect(useChatStore.getState().agents.Arty.connectionPhase).toBe("streaming");
  });

  it("write is a no-op after dispose", () => {
    const s = streamSessionManager.start("Arty");
    s.dispose();
    s.write({ connectionPhase: "streaming" });
    // dispose() itself writes connectionPhase: "idle" as the final legal write.
    expect(useChatStore.getState().agents.Arty.connectionPhase).toBe("idle");
  });

  it("write is a no-op when a new session superseded us", () => {
    const s1 = streamSessionManager.start("Arty");
    streamSessionManager.start("Arty"); // bumps generation, disposes s1
    s1.write({ connectionPhase: "streaming" });
    // s2 is fresh; connectionPhase should be "idle" (the dispose-write landed for s1, no writes from s2 yet)
    expect(useChatStore.getState().agents.Arty.connectionPhase).toBe("idle");
  });

  it("writeDraft hands back the agent's draft, not root state", () => {
    const s = streamSessionManager.start("Arty");
    s.writeDraft((agent) => { agent.streamError = "test"; });
    expect(useChatStore.getState().agents.Arty.streamError).toBe("test");
  });

  it("dispose is idempotent", () => {
    const s = streamSessionManager.start("Arty");
    s.dispose();
    s.dispose(); // must not throw
    expect(s.disposed).toBe(true);
  });

  it("dispose aborts the signal", () => {
    const s = streamSessionManager.start("Arty");
    expect(s.signal.aborted).toBe(false);
    s.dispose();
    expect(s.signal.aborted).toBe(true);
  });

  it("streamSessionManager.start disposes previous session for same agent", () => {
    const s1 = streamSessionManager.start("Arty");
    const s2 = streamSessionManager.start("Arty");
    expect(s1.disposed).toBe(true);
    expect(s2.disposed).toBe(false);
  });

  it("start bumps generation exactly once per logical transition", () => {
    const g0 = useChatStore.getState().agents.Arty.streamGeneration;
    streamSessionManager.start("Arty");
    const g1 = useChatStore.getState().agents.Arty.streamGeneration;
    expect(g1).toBe(g0 + 1);
  });

  it("disposeCurrent bumps generation exactly once (no-op if no active)", () => {
    const g0 = useChatStore.getState().agents.Arty.streamGeneration;
    streamSessionManager.start("Arty");
    const g1 = useChatStore.getState().agents.Arty.streamGeneration;
    streamSessionManager.disposeCurrent("Arty");
    const g2 = useChatStore.getState().agents.Arty.streamGeneration;
    expect(g1).toBe(g0 + 1);
    expect(g2).toBe(g1 + 1);
    streamSessionManager.disposeCurrent("Arty"); // no-op when no active
    const g3 = useChatStore.getState().agents.Arty.streamGeneration;
    expect(g3).toBe(g2);
  });

  it("dev-mode debug log fires on dropped write", () => {
    const spy = vi.spyOn(console, "debug").mockImplementation(() => {});
    const s = streamSessionManager.start("Arty");
    s.dispose();
    s.write({ connectionPhase: "streaming" });
    expect(spy).toHaveBeenCalledOnce();
    spy.mockRestore();
  });

  describe("StreamSession.commit()", () => {
    it("commit() upserts assistant message into live overlay with correct parts", () => {
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = { mode: "live", messages: [] };
      });
      s.buffer.parts.push({
        type: "tool",
        toolCallId: "t1",
        toolName: "fn",
        state: "output-available" as const,
        input: {},
        output: "result",
      });
      s.commit();
      const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
      expect(msgs).toHaveLength(1);
      expect(msgs[0].role).toBe("assistant");
      expect(msgs[0].parts[0].type).toBe("tool");
    });

    it("commit() attributes the live message to the agent name, not the session UUID", () => {
      // Regression: activeSessionId (a UUID) leaked into the buffer's initial
      // responding-agent, so a part committed before the first SSE agentName got
      // agentId = <session UUID> — displayAgentName then masked it as the generic
      // "Агент" label and MessageList drew a spurious agent-transition divider.
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.activeSessionId = "16ba9b12-421d-4bda-ac33-74724c537e1a";
      });
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = { mode: "live", messages: [] };
      });
      s.buffer.parser.processDelta("hi");
      s.commit();
      const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
      expect(msgs).toHaveLength(1);
      expect(msgs[0].agentId).toBe("Arty");
    });

    it("commit() writes message and connectionPhase atomically", () => {
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = { mode: "live", messages: [] };
      });
      s.buffer.parser.processDelta("hello");
      s.commit("idle");
      const agentState = useChatStore.getState().agents.Arty;
      const msgs = getLiveMessages(agentState.messageSource);
      expect(msgs).toHaveLength(1);
      expect(agentState.connectionPhase).toBe("idle");
    });

    it("commit() is a no-op after dispose", () => {
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = { mode: "live", messages: [] };
      });
      s.dispose();
      s.buffer.parser.processDelta("should not appear");
      s.commit();
      const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
      expect(msgs).toHaveLength(0);
    });

    it("commit(\"error\") sets connectionPhase to error", () => {
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = { mode: "live", messages: [] };
        draft.agents.Arty.connectionPhase = "streaming";
      });
      s.commit("error");
      expect(useChatStore.getState().agents.Arty.connectionPhase).toBe("error");
    });

    it("commit() sets status \"streaming\" on the newly-pushed live message (caret can render)", () => {
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = { mode: "live", messages: [] };
      });
      s.buffer.parser.processDelta("hello");
      s.commit();
      const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
      expect(msgs).toHaveLength(1);
      expect(msgs[0].status).toBe("streaming");
    });

    it("commit() sets status \"streaming\" on the update path (existing live message)", () => {
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = { mode: "live", messages: [] };
      });
      s.buffer.parser.processDelta("hello");
      s.commit();
      s.buffer.parser.processDelta(" world");
      s.commit();
      const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
      expect(msgs).toHaveLength(1);
      expect(msgs[0].status).toBe("streaming");
    });

    it("commit() does not downgrade a message already marked \"complete\"", () => {
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = {
          mode: "live",
          messages: [{
            id: s.buffer.assistantId,
            role: "assistant",
            parts: [],
            status: "complete",
          }],
        };
      });
      s.buffer.parser.processDelta("more text after finish");
      s.commit();
      const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
      expect(msgs).toHaveLength(1);
      expect(msgs[0].status).toBe("complete");
    });
  });

  describe("StreamSession.scheduleCommit() / cancelScheduledCommit()", () => {
    it("cancelScheduledCommit() prevents a scheduled commit from firing", async () => {
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = { mode: "live", messages: [] };
      });
      s.buffer.parser.processDelta("should not appear");
      s.scheduleCommit();
      s.cancelScheduledCommit();
      await new Promise(r => setTimeout(r, STREAM_THROTTLE_MS + 60));
      const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
      expect(msgs).toHaveLength(0);
    });

    it("dispose() cancels a pending scheduleCommit", async () => {
      const s = streamSessionManager.start("Arty");
      useChatStore.setState((draft: any) => {
        draft.agents.Arty.messageSource = { mode: "live", messages: [] };
      });
      s.buffer.parser.processDelta("should not appear");
      s.scheduleCommit();
      s.dispose();
      await new Promise(r => setTimeout(r, STREAM_THROTTLE_MS + 60));
      const msgs = getLiveMessages(useChatStore.getState().agents.Arty.messageSource);
      expect(msgs).toHaveLength(0);
    });
  });
});
