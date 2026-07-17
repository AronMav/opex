/**
 * Wave-2 Task 7: bookmark star (MessageActions) + palette "Favourites" section
 * (SearchPalette, empty-query state).
 *
 * (a) star: optimistic toggle before the PATCH resolves, revert + toast on
 *     failure, in-flight guard against overlapping double-click PATCHes.
 * (b) palette empty query renders a Favourites section from listBookmarked
 *     (limit 20), with preview text, NO stacked empty state, and an agent
 *     badge in all-agents mode.
 * (c) clicking a favourite sets the palette target and navigates.
 * (d) a favourite whose session has vanished (REAL 404 on the existence
 *     probe) toasts session_deleted; a transient probe failure (network
 *     reject / 5xx) toasts open_error instead — neither navigates.
 * (e) reopening the palette clears stale favourites (no cross-scope flash).
 */
import { vi, describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// ── Polyfill: ResizeObserver (not available in jsdom, Radix primitives poke it) ──

globalThis.ResizeObserver = class ResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof globalThis.ResizeObserver;

// ── Mock: translation hook (identity) ───────────────────────────────────────

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

// ── Mock: sonner toast ───────────────────────────────────────────────────────

const mockToastError = vi.fn();
vi.mock("sonner", () => ({
  toast: { error: (...args: unknown[]) => mockToastError(...args), success: vi.fn(), info: vi.fn() },
}));

// ── Mock: search-api (toggleBookmark + listBookmarked) ──────────────────────

const mockToggleBookmark = vi.fn();
const mockListBookmarked = vi.fn();
const mockSearchAll = vi.fn();
vi.mock("@/lib/search-api", () => ({
  toggleBookmark: (...args: unknown[]) => mockToggleBookmark(...args),
  listBookmarked: (...args: unknown[]) => mockListBookmarked(...args),
  searchAll: (...args: unknown[]) => mockSearchAll(...args),
}));

// ── Mock: @/lib/query-client ─────────────────────────────────────────────────

const mockInvalidateQueries = vi.fn();
vi.mock("@/lib/query-client", () => ({
  queryClient: { invalidateQueries: (...args: unknown[]) => mockInvalidateQueries(...args) },
}));

// ── Mock: @/lib/queries — self-contained stub (avoids pulling in the real
// module's other store/hook imports). `qk.sessionMessages` mirrors the real
// key builder's shape exactly. `useProviderActive` stubs out SpeakButton's
// react-query hook (only exercised when showReload renders it). ────────────

vi.mock("@/lib/queries", () => ({
  useProviderActive: () => ({ data: [], isLoading: false, error: null }),
  qk: {
    sessionMessages: (id: string) => ["sessions", id, "messages"] as const,
  },
}));

// ── Mock: @/hooks/use-profiles — ReloadButton's model-picker source (13a).
// Stubbed wholesale (no models) so the showReload branch doesn't need a real
// QueryClientProvider; these tests only exercise the bookmark star.
vi.mock("@/hooks/use-profiles", () => ({
  useAgentModelOptions: () => ({ models: [], defaultModel: "" }),
}));

// ── Mock: @/lib/api (session-exists probe for palette favourites + assertToken) ──
// apiFetchRaw is what SearchPalette uses for the probe — Response-like objects
// let tests distinguish ok / 404 / 5xx / network-reject outcomes.

const mockApiFetchRaw = vi.fn();
vi.mock("@/lib/api", () => ({
  apiFetchRaw: (...args: unknown[]) => mockApiFetchRaw(...args),
  assertToken: () => "test-token",
}));

// ── Mock: chat-store ─────────────────────────────────────────────────────────

const AGENT = "Agent1";
const SESSION = "sess-1";
const mockSelectSession = vi.fn();
vi.mock("@/stores/chat-store", () => ({
  useChatStore: Object.assign(
    (selector: (s: Record<string, unknown>) => unknown) => {
      const state = {
        currentAgent: AGENT,
        agents: {
          [AGENT]: {
            activeSessionId: SESSION,
            messageSource: { mode: "live", messages: [] },
          },
        },
      };
      return selector(state);
    },
    {
      getState: () => ({
        currentAgent: AGENT,
        selectSession: mockSelectSession,
      }),
    },
  ),
}));

// ── Mock: next/navigation (palette tests only) ──────────────────────────────

let mockPathname = "/chat/";
const mockPush = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: mockPush }),
  usePathname: () => mockPathname,
}));

import { MessageActions } from "@/app/(authenticated)/chat/MessageActions";
import type { ChatMessage } from "@/stores/chat-types";
import { qk } from "@/lib/queries";
import { SearchPalette } from "@/components/chat/SearchPalette";
import { usePaletteStore } from "@/stores/palette-store";

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

function baseMessage(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "m1",
    role: "assistant",
    parts: [{ type: "text", text: "hello" }],
    bookmarkedAt: null,
    ...overrides,
  };
}

describe("BookmarkButton (MessageActions)", () => {
  beforeEach(() => {
    mockToggleBookmark.mockReset();
    mockInvalidateQueries.mockReset();
    mockToastError.mockReset();
  });

  it("optimistically flips the icon before the PATCH resolves", async () => {
    let resolveToggle!: () => void;
    mockToggleBookmark.mockReturnValue(new Promise<void>((resolve) => { resolveToggle = resolve; }));

    render(<MessageActions message={baseMessage()} showReload={false} />);

    const button = screen.getByRole("button", { name: "chat.bookmark_tooltip" });
    fireEvent.click(button);

    // Icon/aria-label flips immediately — BEFORE the mocked PATCH resolves.
    expect(screen.getByRole("button", { name: "chat.unbookmark_tooltip" })).toBeInTheDocument();
    expect(mockToggleBookmark).toHaveBeenCalledWith("m1", AGENT, true);

    resolveToggle();
    await flush();

    expect(mockInvalidateQueries).toHaveBeenCalledWith({ queryKey: qk.sessionMessages(SESSION) });
    // Stays bookmarked after success.
    expect(screen.getByRole("button", { name: "chat.unbookmark_tooltip" })).toBeInTheDocument();
  });

  it("reverts the optimistic toggle and toasts on a failed PATCH (e.g. 404)", async () => {
    mockToggleBookmark.mockRejectedValue(new Error("HTTP 404"));

    render(<MessageActions message={baseMessage()} showReload={false} />);

    const button = screen.getByRole("button", { name: "chat.bookmark_tooltip" });
    fireEvent.click(button);
    expect(screen.getByRole("button", { name: "chat.unbookmark_tooltip" })).toBeInTheDocument();

    await flush();

    // Reverted back to the unbookmarked icon/label.
    expect(screen.getByRole("button", { name: "chat.bookmark_tooltip" })).toBeInTheDocument();
    expect(mockToastError).toHaveBeenCalledWith("chat.bookmark_error");
    expect(mockInvalidateQueries).not.toHaveBeenCalled();
  });

  it("renders the star for BOTH showReload branches (user + assistant rows)", () => {
    const { unmount } = render(<MessageActions message={baseMessage()} showReload={false} />);
    expect(screen.getByRole("button", { name: "chat.bookmark_tooltip" })).toBeInTheDocument();
    unmount();

    render(<MessageActions message={baseMessage({ id: "m2" })} showReload />);
    expect(screen.getByRole("button", { name: "chat.bookmark_tooltip" })).toBeInTheDocument();
  });

  it("ignores a second click while a toggle is still in flight (no overlapping PATCHes)", async () => {
    let resolveToggle!: () => void;
    mockToggleBookmark.mockReturnValue(new Promise<void>((resolve) => { resolveToggle = resolve; }));

    render(<MessageActions message={baseMessage()} showReload={false} />);

    fireEvent.click(screen.getByRole("button", { name: "chat.bookmark_tooltip" }));
    // Second (double-)click lands while the first PATCH is still pending.
    fireEvent.click(screen.getByRole("button", { name: "chat.unbookmark_tooltip" }));

    expect(mockToggleBookmark).toHaveBeenCalledTimes(1);

    resolveToggle();
    await flush();

    // After the in-flight toggle settles the guard releases — a new click fires.
    mockToggleBookmark.mockResolvedValue(undefined);
    fireEvent.click(screen.getByRole("button", { name: "chat.unbookmark_tooltip" }));
    expect(mockToggleBookmark).toHaveBeenCalledTimes(2);
    expect(mockToggleBookmark).toHaveBeenLastCalledWith("m1", AGENT, false);
  });
});

describe("SearchPalette — Favourites section (T7)", () => {
  const BOOKMARK_ITEMS = [
    {
      message_id: "bm1",
      session_id: "s1",
      session_title: "Session A",
      agent_id: AGENT,
      preview: "an important note",
      role: "user",
      bookmarked_at: "2026-07-16T00:00:00Z",
    },
  ];

  beforeEach(() => {
    mockListBookmarked.mockReset();
    mockListBookmarked.mockResolvedValue({ items: BOOKMARK_ITEMS });
    mockSearchAll.mockReset();
    mockSearchAll.mockResolvedValue({ sessions: [], messages: [], count: 0 });
    mockApiFetchRaw.mockReset();
    mockSelectSession.mockReset();
    mockPush.mockReset();
    mockToastError.mockReset();
    mockPathname = "/chat/";
    usePaletteStore.setState({ open: true, target: null, highlightedMessageId: null });
  });

  it("renders the Favourites section (limit 20) with a preview, no empty state, and an agent badge in all-agents mode", async () => {
    render(<SearchPalette />);
    await flush();

    expect(mockListBookmarked).toHaveBeenCalledWith({ agent: AGENT, limit: 20 });
    expect(screen.getByText("palette.favourites")).toBeInTheDocument();
    expect(screen.getByText("Session A")).toBeInTheDocument();
    expect(screen.getByText("an important note")).toBeInTheDocument();
    // Critical-fix regression guard: with ≥1 favourite rendered, the
    // "no results" empty state must NOT stack on top of the section.
    expect(screen.queryByText("palette.empty")).not.toBeInTheDocument();
    // Agent badge not shown in per-agent mode.
    expect(screen.queryByText(AGENT)).not.toBeInTheDocument();

    const toggle = screen.getByRole("switch");
    fireEvent.click(toggle);
    await flush();

    expect(mockListBookmarked).toHaveBeenLastCalledWith({ all: true, limit: 20 });
    expect(screen.getByText(AGENT)).toBeInTheDocument();
  });

  it("still renders the empty state when there are no favourites and no query", async () => {
    mockListBookmarked.mockResolvedValue({ items: [] });
    render(<SearchPalette />);
    await flush();

    expect(screen.getByText("palette.empty")).toBeInTheDocument();
    expect(screen.queryByText("palette.favourites")).not.toBeInTheDocument();
  });

  it("hides the favourites section as soon as a query is being typed (1 char, below search threshold)", async () => {
    render(<SearchPalette />);
    await flush();
    expect(screen.getByText("palette.favourites")).toBeInTheDocument();

    // One character: below MIN_QUERY_LEN so no search fires and `result`
    // stays null — the residual-review case where favourites used to linger
    // (mouse-clickable but keyboard-unreachable, stacked under palette.empty).
    fireEvent.change(screen.getByPlaceholderText("palette.placeholder"), { target: { value: "a" } });

    expect(screen.queryByText("palette.favourites")).not.toBeInTheDocument();
    expect(screen.queryByText("Session A")).not.toBeInTheDocument();
    // rows is now empty → the single empty state renders alone (consistent).
    expect(screen.getByText("palette.empty")).toBeInTheDocument();

    // Clearing the query brings favourites straight back (still in state).
    fireEvent.change(screen.getByPlaceholderText("palette.placeholder"), { target: { value: "" } });
    expect(screen.getByText("palette.favourites")).toBeInTheDocument();
    expect(screen.queryByText("palette.empty")).not.toBeInTheDocument();
  });

  it("clicking a favourite verifies the session, then setTarget + navigates", async () => {
    mockApiFetchRaw.mockResolvedValue({ ok: true, status: 200 });
    render(<SearchPalette />);
    await flush();

    const row = screen.getByText("Session A").closest("button");
    expect(row).not.toBeNull();
    fireEvent.mouseDown(row!);
    await flush();

    expect(mockApiFetchRaw).toHaveBeenCalledWith("/api/sessions/s1");
    expect(usePaletteStore.getState().target).toEqual({ sessionId: "s1", messageId: "bm1" });
    expect(mockSelectSession).toHaveBeenCalledWith("s1", AGENT);
    expect(mockPush).not.toHaveBeenCalled();
    expect(mockToastError).not.toHaveBeenCalled();
  });

  it("a vanished session (REAL 404 on GET /api/sessions/{id}) toasts session_deleted and does not navigate", async () => {
    mockApiFetchRaw.mockResolvedValue({ ok: false, status: 404 });
    render(<SearchPalette />);
    await flush();

    const row = screen.getByText("Session A").closest("button");
    fireEvent.mouseDown(row!);
    await flush();

    expect(mockToastError).toHaveBeenCalledWith("palette.session_deleted");
    expect(mockSelectSession).not.toHaveBeenCalled();
    expect(mockPush).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().target).toBeNull();
  });

  it("a transient probe failure (network reject / 5xx) toasts open_error — NOT session_deleted — and does not navigate", async () => {
    // Network-level failure: the fetch itself rejects.
    mockApiFetchRaw.mockRejectedValue(new Error("network down"));
    const { unmount } = render(<SearchPalette />);
    await flush();

    fireEvent.mouseDown(screen.getByText("Session A").closest("button")!);
    await flush();

    expect(mockToastError).toHaveBeenCalledWith("palette.open_error");
    expect(mockToastError).not.toHaveBeenCalledWith("palette.session_deleted");
    expect(mockSelectSession).not.toHaveBeenCalled();
    expect(mockPush).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().target).toBeNull();
    unmount();

    // Server-side failure: a 5xx answer must also read as transient.
    mockToastError.mockReset();
    mockApiFetchRaw.mockResolvedValue({ ok: false, status: 503 });
    usePaletteStore.setState({ open: true, target: null, highlightedMessageId: null });
    render(<SearchPalette />);
    await flush();

    fireEvent.mouseDown(screen.getByText("Session A").closest("button")!);
    await flush();

    expect(mockToastError).toHaveBeenCalledWith("palette.open_error");
    expect(mockToastError).not.toHaveBeenCalledWith("palette.session_deleted");
    expect(mockSelectSession).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().target).toBeNull();
  });

  it("reopening the palette clears stale favourites while the refetch is pending (no flash of the previous scope)", async () => {
    render(<SearchPalette />);
    await flush();
    expect(screen.getByText("Session A")).toBeInTheDocument();

    // Close, then reopen with the refetch still in flight — the stale list
    // must be gone immediately (cleared by the open-reset effect), not shown
    // until the new response lands.
    await act(async () => {
      usePaletteStore.setState({ open: false });
    });
    mockListBookmarked.mockReturnValue(new Promise(() => {})); // never resolves
    await act(async () => {
      usePaletteStore.setState({ open: true });
    });

    expect(screen.queryByText("Session A")).not.toBeInTheDocument();
  });
});
