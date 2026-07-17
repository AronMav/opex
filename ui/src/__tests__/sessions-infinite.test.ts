import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import React from "react";

// Mock @/lib/api before importing the hooks under test.
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(),
  apiPost: vi.fn(),
  apiPut: vi.fn(),
  apiDelete: vi.fn(),
  apiPatch: vi.fn(),
}));

import { apiGet } from "@/lib/api";
import {
  useSessions,
  useAutoPaginateWhileFiltering,
  flatSessionsFromCache,
  sessionsGetNextPageParam,
  patchSessionTitleInPages,
  qk,
  SESSIONS_PAGE_SIZE,
  type SessionsPage,
  type SessionsInfiniteData,
} from "@/lib/queries";
import type { SessionRow } from "@/types/api";

function mkSession(id: string, lastMessageAt: string, title = `title-${id}`): SessionRow {
  return {
    id,
    agent_id: "Agent1",
    user_id: "u1",
    channel: "web",
    chat_scope: null,
    started_at: lastMessageAt,
    last_message_at: lastMessageAt,
    title,
    metadata: null,
    run_status: "done",
    participants: [],
    parent_session_id: null,
    end_reason: null,
  };
}

/** A full page of `n` synthetic sessions dated descending from `startIdx`. */
function mkPage(startIdx: number, n: number, total: number): SessionsPage {
  const sessions = Array.from({ length: n }, (_, i) => {
    const idx = startIdx + i;
    // ISO timestamps strictly descending so keyset order is unambiguous.
    const ts = new Date(2026, 0, 1, 0, 0, 0, 1_000_000 - idx * 1000).toISOString();
    return mkSession(`s${idx}`, ts);
  });
  return { sessions, total };
}

function makeClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

function wrapperFor(qc: QueryClient) {
  return function Wrapper({ children }: { children: React.ReactNode }) {
    return React.createElement(QueryClientProvider, { client: qc }, children);
  };
}

beforeEach(() => {
  vi.clearAllMocks();
});

// ── Pure helpers ─────────────────────────────────────────────────────────────

describe("flatSessionsFromCache", () => {
  it("merges two pages into one flat list, deduping by id, preserving order", () => {
    const a = mkSession("a", "2026-01-03T00:00:00Z");
    const b = mkSession("b", "2026-01-02T00:00:00Z");
    const c = mkSession("c", "2026-01-01T00:00:00Z");
    // b appears at the boundary of both pages — must not be duplicated.
    const data = {
      pages: [
        { sessions: [a, b], total: 3 },
        { sessions: [b, c], total: 3 },
      ],
    };
    const flat = flatSessionsFromCache(data);
    expect(flat.map((s) => s.id)).toEqual(["a", "b", "c"]);
  });

  it("returns [] for undefined / empty cache", () => {
    expect(flatSessionsFromCache(undefined)).toEqual([]);
    expect(flatSessionsFromCache({ pages: [] })).toEqual([]);
  });
});

describe("sessionsGetNextPageParam", () => {
  it("yields the (last_message_at, id) cursor of the last row of a full page", () => {
    const page = mkPage(0, SESSIONS_PAGE_SIZE, 100);
    const cursor = sessionsGetNextPageParam(page);
    const last = page.sessions[page.sessions.length - 1];
    expect(cursor).toEqual({
      before_last_message_at: last.last_message_at,
      before_id: last.id,
    });
  });

  it("returns undefined for a short (final) page", () => {
    const page = mkPage(0, SESSIONS_PAGE_SIZE - 1, 39);
    expect(sessionsGetNextPageParam(page)).toBeUndefined();
  });
});

describe("patchSessionTitleInPages", () => {
  it("patches the matching row in-place and keeps untouched pages referentially stable", () => {
    const page0 = { sessions: [mkSession("a", "2026-01-02T00:00:00Z")], total: 2 };
    const page1 = { sessions: [mkSession("b", "2026-01-01T00:00:00Z")], total: 2 };
    const data: SessionsInfiniteData = { pages: [page0, page1], pageParams: [null, {}] };
    const next = patchSessionTitleInPages(data, "b", "renamed");
    expect(next).toBeDefined();
    // Same page count (no reset to a single page).
    expect(next!.pages).toHaveLength(2);
    // Untouched page keeps identity; touched page is a new object.
    expect(next!.pages[0]).toBe(page0);
    expect(next!.pages[1]).not.toBe(page1);
    expect(next!.pages[1].sessions[0].title).toBe("renamed");
  });
});

// ── Hook: infinite pagination ────────────────────────────────────────────────

describe("useSessions (infinite)", () => {
  it("loads the first page and exposes a flat sessions list + total", async () => {
    vi.mocked(apiGet).mockResolvedValueOnce(mkPage(0, SESSIONS_PAGE_SIZE, 100));

    const { result } = renderHook(() => useSessions("Agent1"), {
      wrapper: wrapperFor(makeClient()),
    });

    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.sessions).toHaveLength(SESSIONS_PAGE_SIZE);
    expect(result.current.total).toBe(100);
    expect(result.current.hasNextPage).toBe(true);
  });

  it("fetchNextPage appends the second page with no duplicates", async () => {
    vi.mocked(apiGet)
      .mockResolvedValueOnce(mkPage(0, SESSIONS_PAGE_SIZE, 100))
      .mockResolvedValueOnce(mkPage(SESSIONS_PAGE_SIZE, SESSIONS_PAGE_SIZE, 100));

    const { result } = renderHook(() => useSessions("Agent1"), {
      wrapper: wrapperFor(makeClient()),
    });
    await waitFor(() => expect(result.current.sessions).toHaveLength(SESSIONS_PAGE_SIZE));

    await act(async () => {
      result.current.fetchNextPage();
    });
    await waitFor(() => expect(result.current.sessions).toHaveLength(SESSIONS_PAGE_SIZE * 2));

    const ids = result.current.sessions.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length); // no dupes

    // Second fetch carried the keyset cursor of the first page's last row.
    const secondUrl = vi.mocked(apiGet).mock.calls[1][0] as string;
    expect(secondUrl).toContain("before_id=s39");
    expect(secondUrl).toContain("before_last_message_at=");
  });

  it("invalidation refetches every loaded page (no collapse to 1) and reflects the new session", async () => {
    // Initial two-page load.
    vi.mocked(apiGet)
      .mockResolvedValueOnce(mkPage(0, SESSIONS_PAGE_SIZE, 100))
      .mockResolvedValueOnce(mkPage(SESSIONS_PAGE_SIZE, SESSIONS_PAGE_SIZE, 100));

    const qc = makeClient();
    const { result } = renderHook(() => useSessions("Agent1"), { wrapper: wrapperFor(qc) });
    await waitFor(() => expect(result.current.sessions).toHaveLength(SESSIONS_PAGE_SIZE));
    await act(async () => { result.current.fetchNextPage(); });
    await waitFor(() => expect(result.current.sessions).toHaveLength(SESSIONS_PAGE_SIZE * 2));

    // A new session appears at the head; both pages refetch on invalidation.
    const created = mkSession("new", "2026-06-01T00:00:00Z");
    const refetchedPage0: SessionsPage = {
      sessions: [created, ...mkPage(0, SESSIONS_PAGE_SIZE - 1, 101).sessions],
      total: 101,
    };
    vi.mocked(apiGet)
      .mockResolvedValueOnce(refetchedPage0)
      .mockResolvedValueOnce(mkPage(SESSIONS_PAGE_SIZE - 1, SESSIONS_PAGE_SIZE, 101));

    await act(async () => {
      await qc.invalidateQueries({ queryKey: qk.sessions("Agent1") });
    });

    // Still two pages worth of rows — invalidation did NOT reset to a single page.
    await waitFor(() => expect(result.current.sessions.length).toBeGreaterThan(SESSIONS_PAGE_SIZE));
    expect(result.current.sessions[0].id).toBe("new");
    const data = qc.getQueryData<SessionsInfiniteData>(qk.sessions("Agent1"));
    expect(data!.pages).toHaveLength(2);
  });
});

// ── endReached / filter auto-pagination ──────────────────────────────────────

describe("useAutoPaginateWhileFiltering", () => {
  it("keeps paginating while a filter is active and few rows are visible", () => {
    const fetchNextPage = vi.fn();
    renderHook(() =>
      useAutoPaginateWhileFiltering({
        filterActive: true,
        visibleCount: 2,
        hasNextPage: true,
        isFetchingNextPage: false,
        fetchNextPage,
      }),
    );
    expect(fetchNextPage).toHaveBeenCalledTimes(1);
  });

  it("does NOT paginate when no filter is active (Virtuoso endReached drives that)", () => {
    const fetchNextPage = vi.fn();
    renderHook(() =>
      useAutoPaginateWhileFiltering({
        filterActive: false,
        visibleCount: 2,
        hasNextPage: true,
        isFetchingNextPage: false,
        fetchNextPage,
      }),
    );
    expect(fetchNextPage).not.toHaveBeenCalled();
  });

  it("does NOT re-fire while a fetch is already in flight", () => {
    const fetchNextPage = vi.fn();
    renderHook(() =>
      useAutoPaginateWhileFiltering({
        filterActive: true,
        visibleCount: 2,
        hasNextPage: true,
        isFetchingNextPage: true,
        fetchNextPage,
      }),
    );
    expect(fetchNextPage).not.toHaveBeenCalled();
  });
});
