/**
 * Task 3: branch-aware jump-to-message.
 *
 * The hook consumes palette-store.target: switches to the branch that contains
 * the target message, backfills older history pages if needed, raises the
 * render limit so the row enters the virtualised window, scrolls to it and
 * flashes a highlight. Silent mode (scroll-restore, Task 13c) does none of the
 * user-facing feedback and fails quietly.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import type { MessageRow } from "@/types/api";

// ── Hoisted mock state ────────────────────────────────────────────────────────
// vi.mock factories are hoisted above module-level consts, so any variable a
// factory references must live in vi.hoisted().
const h = vi.hoisted(() => ({
  cachedRows: [] as MessageRow[],
  scrollSpy: vi.fn(),
  toastError: vi.fn(),
  toastInfo: vi.fn(),
  apiGet: vi.fn(),
  cancelQueries: vi.fn((_opts?: { queryKey: unknown }) => Promise.resolve()),
}));

// t() → key identity so toast-copy assertions can key off i18n ids directly
// (open_error vs too_deep). The real hook otherwise resolves live translations.
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

// ── Controllable raw-message cache ────────────────────────────────────────────
// getCachedRawMessages(sessionId, agent) reads queryClient.getQueriesData; point
// it at a mutable fixture so tests can stage which rows are "loaded".
// setQueryData mimics real QueryClient semantics (applies the updater against
// the current fixture and writes the result back) so prependOlderRawMessages
// (Finding 1 fix) is exercised faithfully across backfill loop iterations.
vi.mock("@/lib/query-client", () => ({
  queryClient: {
    getQueriesData: vi.fn(() => [[["x"], { messages: h.cachedRows }]]),
    getQueryData: vi.fn(() => undefined),
    setQueryData: vi.fn((_key: unknown, updater: unknown) => {
      const prev = { messages: h.cachedRows };
      const next = typeof updater === "function"
        ? (updater as (old: typeof prev) => typeof prev | undefined)(prev)
        : (updater as typeof prev | undefined);
      if (next) h.cachedRows = next.messages;
    }),
    invalidateQueries: vi.fn(),
    cancelQueries: h.cancelQueries,
  },
}));

// chat-store pulls these in at import time.
vi.mock("@/stores/streaming-renderer", () => ({
  createStreamingRenderer: () => ({
    sendTurn: vi.fn(),
    connect: vi.fn(),
    resumeStream: vi.fn(),
    abortActiveStream: vi.fn(),
    abortLocalOnly: vi.fn(),
    cleanupAgent: vi.fn(),
    onSessionId: vi.fn(),
  }),
}));

vi.mock("@/lib/api", () => ({
  apiGet: h.apiGet,
  apiPost: vi.fn().mockResolvedValue({}),
  apiPut: vi.fn().mockResolvedValue({}),
  apiPatch: vi.fn().mockResolvedValue({}),
  apiDelete: vi.fn().mockResolvedValue(undefined),
  getToken: () => "t",
  assertToken: () => "t",
}));

// Imperative scroll registry — spy on the scroll call.
vi.mock("../../message-list-handle", () => ({
  setVirtuosoHandle: vi.fn(),
  scrollToMessageIndex: (i: number) => h.scrollSpy(i),
}));

// Toasts.
vi.mock("sonner", () => ({ toast: { error: h.toastError, info: h.toastInfo } }));

import { useChatStore } from "@/stores/chat-store";
import { emptyAgentState } from "@/stores/chat-types";
import type { ChatMessage } from "@/stores/chat-types";
import { usePaletteStore } from "@/stores/palette-store";
import { useScrollToMessage } from "../use-scroll-to-message";

const AGENT = "main";
const SID = "S1";

function row(overrides: Partial<MessageRow> & { id: string }): MessageRow {
  return {
    role: "assistant",
    content: "",
    tool_calls: null,
    tool_call_id: null,
    created_at: "2026-04-21T00:00:00Z",
    agent_id: AGENT,
    feedback: null,
    edited_at: null,
    status: "done",
    thinking_blocks: null,
    parent_message_id: null,
    branch_from_message_id: null,
    abort_reason: null,
    ...overrides,
  } as MessageRow;
}

function chatMsg(id: string, role: "user" | "assistant" = "assistant"): ChatMessage {
  return { id, role, parts: [] };
}

function seed(phase: "idle" | "streaming" = "idle", hasMoreHistory = false) {
  useChatStore.setState({
    currentAgent: AGENT,
    agents: {
      [AGENT]: {
        ...emptyAgentState(),
        activeSessionId: SID,
        connectionPhase: phase,
        messageSource: { mode: "history", sessionId: SID },
        selectedBranches: {},
        hasMoreHistory,
      },
    },
  });
}

beforeEach(() => {
  h.cachedRows = [];
  h.scrollSpy.mockClear();
  h.toastError.mockClear();
  h.toastInfo.mockClear();
  h.apiGet.mockReset().mockResolvedValue({});
  h.cancelQueries.mockClear();
  usePaletteStore.setState({ target: null, highlightedMessageId: null });
});

describe("useScrollToMessage", () => {
  it("switches to the branch that holds the target, then scrolls to it", async () => {
    // Tree: user u1 → assistant a1 (branch A) / a2 (branch B). Default active
    // path picks the latest child (a2); target a1 is on the inactive branch.
    h.cachedRows = [
      row({ id: "u1", role: "user", content: "hi", parent_message_id: null, created_at: "2026-04-21T00:00:00Z" }),
      row({ id: "a1", parent_message_id: "u1", created_at: "2026-04-21T00:00:01Z" }),
      row({ id: "a2", parent_message_id: "u1", branch_from_message_id: "a1", created_at: "2026-04-21T00:00:02Z" }),
    ];
    seed("idle");
    usePaletteStore.getState().setTarget({ sessionId: SID, messageId: "a1" });

    const { rerender } = renderHook(
      ({ msgs }: { msgs: ChatMessage[] }) => useScrollToMessage(AGENT, SID, msgs, true),
      { initialProps: { msgs: [chatMsg("u1", "user"), chatMsg("a2")] } },
    );

    // Branch B is inactive after resolution — the fork parent now points at a1.
    expect(useChatStore.getState().agents[AGENT]?.selectedBranches).toEqual({ u1: "a1" });

    // Re-render as the resolved branch (a1 now in the window) → hook scrolls.
    await act(async () => {
      rerender({ msgs: [chatMsg("u1", "user"), chatMsg("a1")] });
    });
    expect(h.scrollSpy).toHaveBeenCalledWith(1);
    // Highlight flashed on the target.
    expect(usePaletteStore.getState().highlightedMessageId).toBe("a1");
    // Target consumed.
    expect(usePaletteStore.getState().target).toBeNull();
  });

  it("toasts and clears the target when the message is absent and no more history", async () => {
    h.cachedRows = [row({ id: "x1", role: "user", content: "hi" })];
    seed("idle", /* hasMoreHistory */ false);
    usePaletteStore.getState().setTarget({ sessionId: SID, messageId: "gone" });

    await act(async () => {
      renderHook(() => useScrollToMessage(AGENT, SID, [chatMsg("x1", "user")], true));
    });

    // Genuine exhaustion (short page) → the "too far back" copy.
    expect(h.toastError).toHaveBeenCalledWith("palette.too_deep");
    expect(usePaletteStore.getState().target).toBeNull();
    expect(h.scrollSpy).not.toHaveBeenCalled();
  });

  // T3: a backfill FETCH FAILURE is transient, not proof the message is too far
  // back — it must surface the generic error copy, not too_deep.
  it("shows the generic error (not too_deep) when a backfill fetch fails", async () => {
    h.cachedRows = [row({ id: "a1", parent_message_id: "u1", created_at: "2026-04-21T00:00:01Z" })];
    h.apiGet.mockReset().mockRejectedValueOnce(new Error("network"));
    seed("idle", false);
    usePaletteStore.getState().setTarget({ sessionId: SID, messageId: "u1" });

    await act(async () => {
      renderHook(() => useScrollToMessage(AGENT, SID, [chatMsg("a1")], true));
    });

    expect(h.toastError).toHaveBeenCalledWith("palette.open_error");
    expect(h.toastError).not.toHaveBeenCalledWith("palette.too_deep");
    expect(usePaletteStore.getState().target).toBeNull();
    expect(h.scrollSpy).not.toHaveBeenCalled();
  });

  it("silent mode never toasts or highlights on failure", async () => {
    h.cachedRows = [row({ id: "x1", role: "user", content: "hi" })];
    seed("idle", false);
    usePaletteStore.getState().setTarget({ sessionId: SID, messageId: "gone", silent: true });

    await act(async () => {
      renderHook(() => useScrollToMessage(AGENT, SID, [chatMsg("x1", "user")], true));
    });

    expect(h.toastError).not.toHaveBeenCalled();
    expect(h.toastInfo).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().highlightedMessageId).toBeNull();
    expect(usePaletteStore.getState().target).toBeNull();
  });

  it("refuses to jump while the agent is streaming", async () => {
    h.cachedRows = [row({ id: "a1", parent_message_id: null })];
    seed("streaming");
    usePaletteStore.getState().setTarget({ sessionId: SID, messageId: "a1" });

    await act(async () => {
      renderHook(() => useScrollToMessage(AGENT, SID, [chatMsg("a1")], true));
    });

    expect(h.toastError).toHaveBeenCalledTimes(1);
    expect(h.scrollSpy).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().target).toBeNull();
  });

  // Finding 1 regression test: the old backfill loop paged via the
  // `loadPreviousMessages` store action, which reads live-mode messages and
  // no-ops in `mode:"history"` (the mode `seed()` sets up, matching the real
  // palette flow) — `hasMoreHistory` never advanced and a FALSE "too deep"
  // toast fired even though older history existed. The fix pages the React
  // Query cache directly, so this must succeed in history mode with
  // `hasMoreHistory: false` staged (proving that field is no longer consulted).
  it("pages the RQ cache directly to find a target absent from the first page (history mode)", async () => {
    // Only the recent page is "loaded"; the target (root user message) is one
    // page further back and must be fetched via a direct older-page request.
    h.cachedRows = [
      row({ id: "a1", parent_message_id: "u1", created_at: "2026-04-21T00:00:01Z" }),
    ];
    h.apiGet.mockResolvedValueOnce({
      messages: [
        row({ id: "u1", role: "user", content: "hi", parent_message_id: null, created_at: "2026-04-21T00:00:00Z" }),
      ],
      has_more: false,
    });
    seed("idle", /* hasMoreHistory */ false);
    usePaletteStore.getState().setTarget({ sessionId: SID, messageId: "u1" });

    let rerender!: (props: { msgs: ChatMessage[] }) => void;
    await act(async () => {
      const result = renderHook(
        ({ msgs }: { msgs: ChatMessage[] }) => useScrollToMessage(AGENT, SID, msgs, true),
        { initialProps: { msgs: [chatMsg("a1")] } },
      );
      rerender = result.rerender;
    });

    // Fetched exactly one older page anchored on the oldest cached row.
    expect(h.apiGet).toHaveBeenCalledTimes(1);
    expect(h.apiGet.mock.calls[0][0]).toContain("before_id=a1");
    // Prepended into the cache (older row first).
    expect(h.cachedRows.map((r) => r.id)).toEqual(["u1", "a1"]);
    expect(usePaletteStore.getState().target).toBeNull();
    expect(h.toastError).not.toHaveBeenCalled();

    // Re-render as the now-loaded branch (u1 in the window) → hook scrolls.
    await act(async () => {
      rerender({ msgs: [chatMsg("u1", "user"), chatMsg("a1")] });
    });
    expect(h.scrollSpy).toHaveBeenCalledWith(0);
    expect(usePaletteStore.getState().highlightedMessageId).toBe("u1");
  });

  // Finding 2 regression test: branch picks must not be applied if a stream
  // started during the (possibly multi-await) backfill.
  it("refuses to apply picks if streaming starts mid-backfill", async () => {
    h.cachedRows = [
      row({ id: "a1", parent_message_id: "u1", created_at: "2026-04-21T00:00:01Z" }),
    ];
    let resolveFetch!: (v: unknown) => void;
    h.apiGet.mockReturnValueOnce(new Promise((resolve) => { resolveFetch = resolve; }));
    seed("idle", false);
    usePaletteStore.getState().setTarget({ sessionId: SID, messageId: "u1" });

    await act(async () => {
      renderHook(() => useScrollToMessage(AGENT, SID, [chatMsg("a1")], true));
    });

    // A turn starts while the backfill fetch is still in flight.
    useChatStore.setState((draft) => {
      const st = draft.agents[AGENT];
      if (st) st.connectionPhase = "streaming";
    });

    await act(async () => {
      resolveFetch({
        messages: [
          row({ id: "u1", role: "user", content: "hi", parent_message_id: null, created_at: "2026-04-21T00:00:00Z" }),
        ],
        has_more: false,
      });
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(h.toastError).toHaveBeenCalledTimes(1);
    expect(h.scrollSpy).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().target).toBeNull();
    // No branch picks applied — the resolution was refused, not completed.
    expect(useChatStore.getState().agents[AGENT]?.selectedBranches).toEqual({});
  });

  // C1 regression test: on a cold RQ cache selectSession flips activeSessionId
  // synchronously, so the resolution effect can fire BEFORE the session's first
  // history page is fetched. Without the historyReady gate, getCachedRawMessages
  // is [] → no anchor row → false "too_deep" toast + target consumed. The gate
  // must defer resolution (target intact, no attempt spent) until page 1 lands.
  it("does not consume the target on a cold cache; resolves once the first page lands (historyReady)", async () => {
    h.cachedRows = []; // first page not fetched yet
    seed("idle", false);
    usePaletteStore.getState().setTarget({ sessionId: SID, messageId: "a1" });

    let rerender!: (props: { ready: boolean; msgs: ChatMessage[] }) => void;
    await act(async () => {
      const result = renderHook(
        ({ ready, msgs }: { ready: boolean; msgs: ChatMessage[] }) =>
          useScrollToMessage(AGENT, SID, msgs, ready),
        { initialProps: { ready: false, msgs: [] as ChatMessage[] } },
      );
      rerender = result.rerender;
    });

    // Not ready → nothing consumed, no fetch, no toast, no scroll.
    expect(usePaletteStore.getState().target).not.toBeNull();
    expect(h.apiGet).not.toHaveBeenCalled();
    expect(h.toastError).not.toHaveBeenCalled();
    expect(h.scrollSpy).not.toHaveBeenCalled();

    // First page arrives: cache populated + historyReady flips true.
    h.cachedRows = [row({ id: "a1", parent_message_id: null })];
    await act(async () => {
      rerender({ ready: true, msgs: [chatMsg("a1")] });
    });

    expect(h.scrollSpy).toHaveBeenCalledWith(0);
    expect(usePaletteStore.getState().highlightedMessageId).toBe("a1");
    expect(usePaletteStore.getState().target).toBeNull();
    expect(h.toastError).not.toHaveBeenCalled();
  });

  // I3 regression test: navigation.ts invalidates sessionMessages on every
  // selectSession; a refetch landing mid-backfill would replace the prepended
  // cache with one fresh page and orphan the pending scroll. The hook cancels
  // in-flight refetches before/after each page so the backfill survives to
  // resolution — assert cancelQueries is wired and the scroll still lands.
  it("cancels concurrent refetches during backfill so the prepend survives (invalidate race)", async () => {
    h.cachedRows = [row({ id: "a1", parent_message_id: "u1", created_at: "2026-04-21T00:00:01Z" })];
    h.apiGet.mockResolvedValueOnce({
      messages: [
        row({ id: "u1", role: "user", content: "hi", parent_message_id: null, created_at: "2026-04-21T00:00:00Z" }),
      ],
      has_more: false,
    });
    seed("idle", false);
    usePaletteStore.getState().setTarget({ sessionId: SID, messageId: "u1" });

    let rerender!: (props: { msgs: ChatMessage[] }) => void;
    await act(async () => {
      const result = renderHook(
        ({ msgs }: { msgs: ChatMessage[] }) => useScrollToMessage(AGENT, SID, msgs, true),
        { initialProps: { msgs: [chatMsg("a1")] } },
      );
      rerender = result.rerender;
    });

    // Guard wired: refetches for this session's messages were cancelled.
    expect(h.cancelQueries).toHaveBeenCalled();
    expect(h.cancelQueries.mock.calls[0][0]).toEqual({ queryKey: ["sessions", SID, "messages"] });
    // The backfilled page survived and the resolution completed deterministically.
    expect(h.cachedRows.map((r) => r.id)).toEqual(["u1", "a1"]);
    expect(usePaletteStore.getState().target).toBeNull();
    expect(h.toastError).not.toHaveBeenCalled();

    await act(async () => {
      rerender({ msgs: [chatMsg("u1", "user"), chatMsg("a1")] });
    });
    expect(h.scrollSpy).toHaveBeenCalledWith(0);
  });
});
