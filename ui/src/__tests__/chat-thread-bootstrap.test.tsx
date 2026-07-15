import { describe, it, expect, vi, beforeEach } from "vitest";
import { render } from "@testing-library/react";
import { useEffect } from "react";
import { useChatStore } from "@/stores/chat-store";
import { isActivePhase } from "@/stores/chat-types";

// We don't render the full ChatThread (heavy and pulls in many providers); just
// isolate the bootstrap effect logic in a small harness component. The effect
// being tested is the T8 unconditional-connect bootstrap in ChatThread.tsx.
const resumeStreamSpy = vi.fn();

function BootstrapHarness({ agent }: { agent: string }) {
  const activeSessionId = useChatStore((s) => s.agents[agent]?.activeSessionId ?? null);
  const connectionPhase = useChatStore((s) => s.agents[agent]?.connectionPhase ?? "idle");

  // Mirror of the T8 bootstrap effect from ChatThread.tsx: on session change,
  // unconditionally open the single connect path unless already active. The
  // server replays the in-flight envelope or an empty finished envelope.
  useEffect(() => {
    if (!activeSessionId || isActivePhase(connectionPhase)) return;
    useChatStore.getState().resumeStream(agent, activeSessionId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSessionId, agent]);

  return null;
}

describe("ChatThread bootstrap effect (T8 unconditional connect)", () => {
  beforeEach(() => {
    resumeStreamSpy.mockClear();
    useChatStore.setState({
      resumeStream: resumeStreamSpy,
      agents: {
        alpha: {
          activeSessionId: "s1",
          connectionPhase: "idle",
          activeSessionIds: ["s1"],
        } as any,
      },
    } as any);
  });

  it("triggers connect on mount when idle with a session", () => {
    render(<BootstrapHarness agent="alpha" />);
    expect(resumeStreamSpy).toHaveBeenCalledWith("alpha", "s1");
    expect(resumeStreamSpy).toHaveBeenCalledTimes(1);
  });

  it("does NOT trigger connect when phase is already active (streaming)", () => {
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

  it("triggers connect even when the session is NOT in the WS activeSessionIds snapshot", () => {
    // Server-authoritative (T8): no WS gate — connect always fires, and the
    // server returns an empty finished envelope if there is no in-flight turn.
    useChatStore.setState({
      agents: {
        alpha: {
          activeSessionId: "s1",
          connectionPhase: "idle",
          activeSessionIds: [], // WS hasn't marked it
        } as any,
      },
    } as any);
    render(<BootstrapHarness agent="alpha" />);
    expect(resumeStreamSpy).toHaveBeenCalledWith("alpha", "s1");
  });
});
