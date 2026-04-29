"use client";

import { vi, describe, it, expect, beforeEach } from "vitest";
import React from "react";
import { render } from "@testing-library/react";

// ── Mocks must appear before any imports under test ───────────────────────

vi.mock("@/hooks/use-visual-viewport", () => ({
  useVisualViewport: () => 0,
}));

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/chat",
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

// ── Mock chat sub-hooks ───────────────────────────────────────────────────

vi.mock("../hooks/use-engine-running", () => ({
  useEngineRunning: () => false,
}));

vi.mock("../hooks/use-render-messages", () => ({
  useRenderMessages: () => [],
}));

vi.mock("../hooks/use-is-live", () => ({
  useIsLive: () => false,
}));

vi.mock("../hooks/use-is-replaying-history", () => ({
  useIsReplayingHistory: () => false,
}));

vi.mock("../hooks/use-live-has-content", () => ({
  useLiveHasContent: () => false,
}));

// ── Mock heavy child components ──────────────────────────────────────────

vi.mock("../MessageList", () => ({
  MessageList: () => null,
  MessageSkeleton: () => null,
}));

vi.mock("../EmptyState", () => ({
  EmptyState: () => null,
}));

vi.mock("../read-only/ReadOnlyFooter", () => ({
  ReadOnlyFooter: () => null,
}));

vi.mock("../error/ErrorBanner", () => ({
  ErrorBanner: () => null,
}));

vi.mock("../composer/ChatComposer", () => ({
  ChatComposer: () => null,
}));

vi.mock("@/components/chat/ReconnectingIndicator", () => ({
  ReconnectingIndicator: () => null,
}));

vi.mock("@/components/chat/ToolCallPartView", () => ({
  ToolCallPartView: () => null,
}));

vi.mock("@/components/chat/FileDataPartView", () => ({
  FileDataPartView: () => null,
}));

// ── Mutable state for chat-store mock ────────────────────────────────────

let connectionPhase = "idle" as string;
let activeSessionIds = ["s1"] as string[];
const resumeStream = vi.fn();

function setPhase(p: string) {
  connectionPhase = p;
}

function setActiveSessionIds(ids: string[]) {
  activeSessionIds = ids;
}

vi.mock("@/stores/chat-store", () => {
  // Re-read mutable closures on every call so re-renders pick up changes.
  const buildState = () => ({
    currentAgent: "Arty",
    agents: {
      Arty: {
        activeSessionId: "s1",
        connectionPhase,
        reconnectAttempt: 0,
        maxReconnectAttempts: 3,
        activeSessionIds,
        renderLimit: 100,
      },
    },
    resumeStream,
    loadEarlierMessages: vi.fn(),
  });

  const useChatStore: any = (selector: any) => selector(buildState());
  useChatStore.getState = buildState;

  return {
    useChatStore,
    isActivePhase: (p: string) =>
      p === "submitted" || p === "streaming" || p === "reconnecting",
  };
});

// ── Mock queries ─────────────────────────────────────────────────────────

vi.mock("@/lib/queries", () => ({
  useSessions: () => ({
    data: { sessions: [{ id: "s1", run_status: "running" }] },
    isLoading: false,
    error: null,
    refetch: vi.fn(),
  }),
  useSessionMessages: () => ({
    data: undefined,
    isLoading: false,
    error: null,
    refetch: vi.fn(),
  }),
}));

// ── Import under test (after mocks) ──────────────────────────────────────

import { ChatThread } from "../ChatThread";

// ── Helpers ───────────────────────────────────────────────────────────────

function renderThread() {
  return render(
    <ChatThread
      streamError={null}
      isReadOnly={false}
      activeSession={undefined}
      onClearError={vi.fn()}
      onRetry={vi.fn()}
    />,
  );
}

// ── Tests ─────────────────────────────────────────────────────────────────

describe("ChatThread resume-effect (QUICK-260421-0w6)", () => {
  beforeEach(() => {
    resumeStream.mockClear();
    connectionPhase = "idle";
    activeSessionIds = ["s1"];
  });

  it("resume effect fires resumeStream on first mount when session is running", () => {
    // connectionPhase="idle", activeSessionIds=["s1"], sessionRunStatus="running" (from useSessions mock)
    const { unmount } = renderThread();
    expect(resumeStream).toHaveBeenCalledTimes(1);
    expect(resumeStream).toHaveBeenCalledWith("Arty", "s1");
    unmount();
  });

  it("submitted→idle (204 response) does NOT clear the guard — prevents stale-cache loop", () => {
    // Regression guard for the blinking-cursor loop:
    //   connectionPhase=idle + sessionRunStatus="running" (stale cache)
    //   → resumeStream fires → connectionPhase=submitted
    //   (old code cleared resumedSessionsRef here because isActivePhase("submitted")=true)
    //   → server returns 204 → connectionPhase=idle
    //   (old code: guard cleared → resumeStream fires again → infinite loop)
    //
    // With the fix, "submitted" phase does NOT clear the guard.
    // Only "streaming" or "reconnecting" (real data flowing) resets it.

    // Step 1: mount with idle+running → first resume call
    const { rerender } = renderThread();
    expect(resumeStream).toHaveBeenCalledTimes(1);

    // Step 2: simulate connectionPhase → submitted (resumeStream has started the fetch)
    setPhase("submitted");
    rerender(
      <ChatThread streamError={null} isReadOnly={false} activeSession={undefined}
        onClearError={vi.fn()} onRetry={vi.fn()} />,
    );
    expect(resumeStream).toHaveBeenCalledTimes(1); // no new calls while active

    // Step 3: simulate 204 response → back to idle WITHOUT going through "streaming"
    setPhase("idle");
    rerender(
      <ChatThread streamError={null} isReadOnly={false} activeSession={undefined}
        onClearError={vi.fn()} onRetry={vi.fn()} />,
    );

    // CRITICAL: must NOT fire again — guard still holds because "submitted" phase
    // did not clear resumedSessionsRef. The React Query cache still shows
    // sessionRunStatus="running" (stale), but the guard blocks the re-trigger.
    expect(resumeStream).toHaveBeenCalledTimes(1);
  });

  it("resume re-fires when the same mounted component sees a second idle→running transition", () => {
    // Render once with connectionPhase="streaming" (effect exits early via isActivePhase)
    setPhase("streaming");
    const { rerender } = renderThread();
    expect(resumeStream).toHaveBeenCalledTimes(0);

    // Switch to idle + isRunning=true → first resume call expected
    setPhase("idle");
    rerender(
      <ChatThread
        streamError={null}
        isReadOnly={false}
        activeSession={undefined}
        onClearError={vi.fn()}
        onRetry={vi.fn()}
      />,
    );
    expect(resumeStream).toHaveBeenCalledTimes(1);
    expect(resumeStream).toHaveBeenLastCalledWith("Arty", "s1");

    // Switch to streaming again → effect exits early
    setPhase("streaming");
    rerender(
      <ChatThread
        streamError={null}
        isReadOnly={false}
        activeSession={undefined}
        onClearError={vi.fn()}
        onRetry={vi.fn()}
      />,
    );
    expect(resumeStream).toHaveBeenCalledTimes(1); // still 1

    // Back to idle + isRunning=true → SECOND resume call expected.
    // BUG: resumedSessions Set.has("s1") still returns true and blocks this call.
    // This test FAILS on current code (RED), PASSES after fix (GREEN).
    setPhase("idle");
    rerender(
      <ChatThread
        streamError={null}
        isReadOnly={false}
        activeSession={undefined}
        onClearError={vi.fn()}
        onRetry={vi.fn()}
      />,
    );
    expect(resumeStream).toHaveBeenCalledTimes(2); // ← fails on current code
    expect(resumeStream).toHaveBeenLastCalledWith("Arty", "s1");
  });
});
