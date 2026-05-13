import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";

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

// Spy that fails the test if useSessions is invoked from inside useEngineRunning.
const useSessionsSpy = vi.fn(() => ({ data: undefined }));
vi.mock("@/lib/queries", () => ({
  useSessions: () => useSessionsSpy(),
}));

import { useEngineRunning } from "@/app/(authenticated)/chat/hooks/use-engine-running";
import { useChatStore } from "@/stores/chat-store";

describe("useEngineRunning (simplified)", () => {
  beforeEach(() => {
    useSessionsSpy.mockClear();
    useChatStore.setState({
      currentAgent: "alpha",
      agents: {
        alpha: {
          activeSessionId: "s1",
          connectionPhase: "idle",
          activeSessionIds: [],
        } as any,
      },
    } as any);
  });

  it("returns true when activeSessionIds contains current session", () => {
    useChatStore.setState({
      agents: {
        alpha: {
          activeSessionId: "s1",
          connectionPhase: "idle",
          activeSessionIds: ["s1"],
        } as any,
      },
    } as any);
    const { result } = renderHook(() => useEngineRunning("alpha"));
    expect(result.current).toBe(true);
  });

  it("returns true during streaming phase", () => {
    useChatStore.setState({
      agents: {
        alpha: {
          activeSessionId: "s1",
          connectionPhase: "streaming",
          activeSessionIds: [],
        } as any,
      },
    } as any);
    const { result } = renderHook(() => useEngineRunning("alpha"));
    expect(result.current).toBe(true);
  });

  it("returns false when WS says idle (no DB fallback)", () => {
    const { result } = renderHook(() => useEngineRunning("alpha"));
    expect(result.current).toBe(false);
  });

  it("does NOT subscribe to useSessions query", () => {
    renderHook(() => useEngineRunning("alpha"));
    expect(useSessionsSpy).not.toHaveBeenCalled();
  });
});
