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
