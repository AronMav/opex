import { describe, it, expect, vi, beforeEach } from "vitest";
import { useChatStore } from "@/stores/chat-store";

const resumeStreamSpy = vi.fn();

describe("markSessionActive auto-resume trigger", () => {
  beforeEach(() => {
    resumeStreamSpy.mockClear();
    useChatStore.setState({
      currentAgent: "alpha",
      agents: {
        alpha: {
          activeSessionId: "s1",
          connectionPhase: "idle",
          activeSessionIds: [],
        } as any,
      },
      resumeStream: resumeStreamSpy,
    } as any);
  });

  it("triggers resumeStream when idle on matching session", () => {
    useChatStore.getState().markSessionActive("alpha", "s1");
    expect(resumeStreamSpy).toHaveBeenCalledWith("alpha", "s1");
  });

  it("does NOT trigger resumeStream when streaming", () => {
    useChatStore.setState({
      agents: {
        alpha: { activeSessionId: "s1", connectionPhase: "streaming",
                 activeSessionIds: [] } as any,
      },
    } as any);
    useChatStore.getState().markSessionActive("alpha", "s1");
    expect(resumeStreamSpy).not.toHaveBeenCalled();
  });

  it("does NOT trigger resumeStream for a different session", () => {
    useChatStore.getState().markSessionActive("alpha", "s2");
    expect(resumeStreamSpy).not.toHaveBeenCalled();
  });

  it("does NOT trigger resumeStream for a different agent", () => {
    useChatStore.getState().markSessionActive("beta", "s1");
    expect(resumeStreamSpy).not.toHaveBeenCalled();
  });
});

// ── Item 1 (2026-07-18): auto-open a running session on the welcome screen ──
// Post-login restore race: setThinking (layout.tsx global WS handler) or the
// page-mount restore effect may leave activeSessionId null / messageSource
// stuck at "new-chat" for the CURRENT agent while a WS "agent_processing"
// snapshot reports a session is genuinely running. markSessionActive must
// open that session (activeSessionId + messageSource) in addition to
// resuming — otherwise the composer shows an active stream over a
// permanently empty welcome screen (ChatPage's restore effect bails out on
// "already streaming, don't touch" before it ever fixes messageSource).
describe("markSessionActive auto-open (welcome-state restore)", () => {
  beforeEach(() => {
    resumeStreamSpy.mockClear();
  });

  it("opens and resumes a running session when nothing is selected yet (welcome state)", () => {
    useChatStore.setState({
      currentAgent: "alpha",
      agents: {
        alpha: {
          activeSessionId: null,
          connectionPhase: "idle",
          activeSessionIds: [],
          messageSource: { mode: "new-chat" },
        } as any,
      },
      resumeStream: resumeStreamSpy,
    } as any);

    useChatStore.getState().markSessionActive("alpha", "r1");

    expect(resumeStreamSpy).toHaveBeenCalledWith("alpha", "r1");
    const st = useChatStore.getState().agents.alpha;
    expect(st.activeSessionId).toBe("r1");
    expect(st.messageSource).toEqual({ mode: "history", sessionId: "r1" });
  });

  it("does NOT hijack an already-selected different session (welcome-state guard is narrow)", () => {
    useChatStore.setState({
      currentAgent: "alpha",
      agents: {
        alpha: {
          activeSessionId: "s_open",
          connectionPhase: "idle",
          activeSessionIds: [],
          messageSource: { mode: "history", sessionId: "s_open" },
        } as any,
      },
      resumeStream: resumeStreamSpy,
    } as any);

    useChatStore.getState().markSessionActive("alpha", "r1");

    expect(resumeStreamSpy).not.toHaveBeenCalled();
    const st = useChatStore.getState().agents.alpha;
    expect(st.activeSessionId).toBe("s_open");
    expect(st.messageSource).toEqual({ mode: "history", sessionId: "s_open" });
  });

  it("does NOT auto-open for an agent the user isn't currently viewing", () => {
    useChatStore.setState({
      currentAgent: "alpha",
      agents: {
        beta: {
          activeSessionId: null,
          connectionPhase: "idle",
          activeSessionIds: [],
          messageSource: { mode: "new-chat" },
        } as any,
      },
      resumeStream: resumeStreamSpy,
    } as any);

    useChatStore.getState().markSessionActive("beta", "r1");

    expect(resumeStreamSpy).not.toHaveBeenCalled();
    const st = useChatStore.getState().agents.beta;
    expect(st.activeSessionId).toBe(null);
    expect(st.messageSource).toEqual({ mode: "new-chat" });
  });

  it("does NOT re-open after an explicit New Chat (forceNewSession set)", () => {
    // newChat() leaves {activeSessionId: null, mode: "new-chat",
    // forceNewSession: true} — the user explicitly abandoned the old
    // session. A WS reconnect snapshot (network blip, mobile wake — ws.ts
    // force-reconnect) re-fires markSessionActive for the still-running-
    // server-side old session; it must NOT pull the user back into it.
    // Mirrors the setThinking guard in composer.ts (`!st?.forceNewSession`).
    useChatStore.setState({
      currentAgent: "alpha",
      agents: {
        alpha: {
          activeSessionId: null,
          connectionPhase: "idle",
          activeSessionIds: [],
          messageSource: { mode: "new-chat" },
          forceNewSession: true,
        } as any,
      },
      resumeStream: resumeStreamSpy,
    } as any);

    useChatStore.getState().markSessionActive("alpha", "old_running");

    expect(resumeStreamSpy).not.toHaveBeenCalled();
    const st = useChatStore.getState().agents.alpha;
    expect(st.activeSessionId).toBe(null);
    expect(st.messageSource).toEqual({ mode: "new-chat" });
    // The running-session bookkeeping itself must still be recorded.
    expect(st.activeSessionIds).toContain("old_running");
  });

  it("fixes messageSource even when activeSessionId was already set to the running session (setThinking race)", () => {
    // Simulates the ordering where layout.tsx's global setThinking handler ran
    // BEFORE this per-page markSessionActive handler for the same WS event:
    // activeSessionId is already the running session, but messageSource is
    // still stuck at "new-chat".
    useChatStore.setState({
      currentAgent: "alpha",
      agents: {
        alpha: {
          activeSessionId: "r1",
          connectionPhase: "idle",
          activeSessionIds: [],
          messageSource: { mode: "new-chat" },
        } as any,
      },
      resumeStream: resumeStreamSpy,
    } as any);

    useChatStore.getState().markSessionActive("alpha", "r1");

    expect(resumeStreamSpy).toHaveBeenCalledWith("alpha", "r1");
    const st = useChatStore.getState().agents.alpha;
    expect(st.messageSource).toEqual({ mode: "history", sessionId: "r1" });
  });
});
