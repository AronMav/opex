import { describe, it, expect, vi, beforeEach } from "vitest";
import { render } from "@testing-library/react";
import { useEffect } from "react";
import { useChatStore } from "@/stores/chat-store";
import { isActivePhase } from "@/stores/chat-types";

// We don't render the full ChatThread (heavy and pulls in many providers); just
// isolate the bootstrap effect logic in a small harness component. The effect
// being tested is the one added in Task 14 to ChatThread.tsx.
const resumeStreamSpy = vi.fn();

function BootstrapHarness({ agent }: { agent: string }) {
  const activeSessionId = useChatStore((s) => s.agents[agent]?.activeSessionId ?? null);
  const connectionPhase = useChatStore((s) => s.agents[agent]?.connectionPhase ?? "idle");
  const activeSessionIds = useChatStore((s) => s.agents[agent]?.activeSessionIds ?? []);

  // Mirror of the bootstrap effect from ChatThread.tsx (Task 14 addition).
  useEffect(() => {
    if (!activeSessionId || isActivePhase(connectionPhase)) return;
    if (activeSessionIds.includes(activeSessionId)) {
      useChatStore.getState().resumeStream(agent, activeSessionId);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSessionId, agent]);

  return null;
}

describe("ChatThread bootstrap effect (T3.6 / R-MED-1)", () => {
  beforeEach(() => {
    resumeStreamSpy.mockClear();
    useChatStore.setState({
      resumeStream: resumeStreamSpy,
      agents: {
        alpha: {
          activeSessionId: "s1",
          connectionPhase: "idle",
          activeSessionIds: ["s1"],  // WS snapshot already pushed
        } as any,
      },
    } as any);
  });

  it("triggers resume on mount when WS already marked session active", () => {
    render(<BootstrapHarness agent="alpha" />);
    expect(resumeStreamSpy).toHaveBeenCalledWith("alpha", "s1");
    expect(resumeStreamSpy).toHaveBeenCalledTimes(1);
  });

  it("does NOT trigger resume when phase is streaming", () => {
    useChatStore.setState({
      agents: {
        alpha: {
          activeSessionId: "s1",
          connectionPhase: "streaming",
          activeSessionIds: ["s1"],
        } as any,
      },
    } as any);
    render(<BootstrapHarness agent="alpha" />);
    expect(resumeStreamSpy).not.toHaveBeenCalled();
  });

  it("does NOT trigger resume when session not in activeSessionIds", () => {
    useChatStore.setState({
      agents: {
        alpha: {
          activeSessionId: "s1",
          connectionPhase: "idle",
          activeSessionIds: [],  // WS hasn't marked yet
        } as any,
      },
    } as any);
    render(<BootstrapHarness agent="alpha" />);
    expect(resumeStreamSpy).not.toHaveBeenCalled();
  });
});
