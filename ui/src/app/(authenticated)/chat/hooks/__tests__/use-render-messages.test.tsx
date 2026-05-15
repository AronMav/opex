import { describe, it, expect, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import React from "react";

// ── Hoist the QueryClient so the vi.mock factory can reference it ─────────────
// vi.mock factories are hoisted before any `const` declarations, so ordinary
// module-level variables are not yet initialised at that point.
// vi.hoisted() runs at the same elevation as the factory.
const { qc } = vi.hoisted(() => {
  const { QueryClient } = require("@tanstack/react-query") as typeof import("@tanstack/react-query");
  const qc = new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        // No-op queryFn: the hook is a read-only subscriber; data arrives via
        // setQueryData (as in production where useSessionMessages fetches).
        queryFn: () => null,
      },
    },
  });
  return { qc };
});

// ── Redirect the singleton queryClient used by getCachedHistoryMessages ───────
// chat-history.ts imports `queryClient` from @/lib/query-client at module scope.
// We must point it to the same instance the QueryClientProvider exposes so that
// setQueryData propagates to both the useQuery subscription and the read-through.
vi.mock("@/lib/query-client", () => ({ queryClient: qc }));

// ── Store mock ────────────────────────────────────────────────────────────────
// Follow the pattern from use-engine-running.test.tsx: expose a controlled
// state object and make useChatStore execute the selector against it.
vi.mock("@/stores/chat-store", () => {
  const state = {
    agents: {
      Arty: {
        messageSource: { mode: "history", sessionId: "sess-1" },
        activeSessionId: "sess-1",
        selectedBranches: {},
      },
    },
  };
  return {
    useChatStore: (selector: (s: typeof state) => unknown) => selector(state),
  };
});

// Import AFTER mocks are in place so the module resolves the mocked deps.
import { qk } from "@/lib/queries";
import { useRenderMessages } from "../use-render-messages";

// ── Wrapper ───────────────────────────────────────────────────────────────────
function wrapper({ children }: { children: React.ReactNode }) {
  return React.createElement(QueryClientProvider, { client: qc }, children);
}

// ── Raw message rows (MessageRow shape from api.generated.ts) ─────────────────
const rawMessages = [
  {
    id: "m1",
    role: "user",
    content: "hello",
    tool_calls: null,
    tool_call_id: null,
    created_at: "2026-04-21T00:00:00Z",
    agent_id: null,
    feedback: null,
    edited_at: null,
    status: "done",
    thinking_blocks: null,
    parent_message_id: null,
    branch_from_message_id: null,
    abort_reason: null,
  },
  {
    id: "m2",
    role: "assistant",
    content: "hi",
    tool_calls: null,
    tool_call_id: null,
    created_at: "2026-04-21T00:00:01Z",
    agent_id: "Arty",
    feedback: null,
    edited_at: null,
    status: "done",
    thinking_blocks: null,
    parent_message_id: null,
    branch_from_message_id: null,
    abort_reason: null,
  },
];

describe("useRenderMessages — RQ cache subscription", () => {
  it("re-renders and surfaces messages when the RQ cache is populated after mount", () => {
    // Clear the session cache to ensure no stale data from prior test runs.
    qc.removeQueries({ queryKey: qk.sessionMessages("sess-1") });

    const { result, rerender } = renderHook(() => useRenderMessages("Arty"), { wrapper });

    // Initially the cache is empty → hook must return [].
    expect(result.current.length).toBe(0);

    // Simulate ChatThread's useSessionMessages populating the RQ cache.
    // None of the other useMemo deps (messageSource, selectedBranches,
    // activeSessionId, agent) change — only the cache fills.
    // The hook (variant C, post-refactor) subscribes to the 4-element key
    // [...qk.sessionMessages(id), agent]; setQueryData must store there so
    // useQuery's dataUpdatedAt actually fires the memo recompute.
    qc.setQueryData([...qk.sessionMessages("sess-1"), "Arty"], {
      messages: rawMessages,
    });

    // Trigger a re-render (e.g. a parent component updating). In the buggy code
    // (no useQuery subscription), useMemo still returns the stale [] because its
    // deps are unchanged. In the fixed code, dataUpdatedAt from useQuery changes
    // on the same re-render cycle, causing useMemo to recompute and surface the
    // newly cached messages.
    rerender();

    // After cache fill + re-render, the hook MUST return the loaded messages.
    expect(result.current.length).toBeGreaterThan(0);
  });
});
