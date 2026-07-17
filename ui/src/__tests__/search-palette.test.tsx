import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// ── Polyfill: ResizeObserver (not available in jsdom, Radix primitives poke it) ──

globalThis.ResizeObserver = class ResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof globalThis.ResizeObserver;

// ── Mock: lucide-react (stub the two icons the palette uses) ───────────────

vi.mock("lucide-react", () => {
  const Icon = () => null;
  return { Loader2: Icon, Search: Icon };
});

// ── Mock: translation hook ───────────────────────────────────────────────────

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

// ── Mock: chat-store (currentAgent read via hook; selectSession via getState,
// house pattern — see ChatThread.voice-drain.test.tsx) ─────────────────────

const mockSelectSession = vi.fn();
vi.mock("@/stores/chat-store", () => ({
  useChatStore: Object.assign(
    (selector: (s: { currentAgent: string }) => unknown) => selector({ currentAgent: "Agent1" }),
    { getState: () => ({ currentAgent: "Agent1", selectSession: mockSelectSession }) },
  ),
}));

// ── Mock: next/navigation (router.push + pathname, mutable per-test) ───────
// The default pathname carries a trailing slash — the REAL runtime value:
// next.config.ts sets `trailingSlash: true` (static export), so production
// usePathname() returns "/chat/", never "/chat".

let mockPathname = "/chat/";
const mockPush = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: mockPush }),
  usePathname: () => mockPathname,
}));

// ── Mock: search-api ─────────────────────────────────────────────────────────
// listBookmarked is fetched on mount (empty-query favourites section, T7) —
// stub it so the pre-existing search tests (which never type an empty query
// on purpose, but DO mount with an empty query before typing) don't crash.

const mockSearchAll = vi.fn();
const mockListBookmarked = vi.fn();
vi.mock("@/lib/search-api", () => ({
  searchAll: (...args: unknown[]) => mockSearchAll(...args),
  listBookmarked: (...args: unknown[]) => mockListBookmarked(...args),
}));

import { SearchPalette } from "@/components/chat/SearchPalette";
import { usePaletteStore } from "@/stores/palette-store";

const SEARCH_RESULT = {
  sessions: [
    { session_id: "s2", title: "Session B", agent_id: "Agent1", last_message_at: "2026-07-16T00:00:00Z" },
  ],
  messages: [
    {
      message_id: "m1",
      content: "hello world",
      session_id: "s1",
      session_title: "Session A",
      agent_id: "Agent1",
      user_id: null,
      channel: null,
      role: "user",
      created_at: "2026-07-16T00:00:00Z",
      rank: 1,
      snippet: "he<b>llo</b> world",
    },
  ],
  count: 1,
};

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("SearchPalette (Ctrl+K)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    mockSearchAll.mockReset();
    mockSearchAll.mockResolvedValue(SEARCH_RESULT);
    mockListBookmarked.mockReset();
    mockListBookmarked.mockResolvedValue({ items: [] });
    mockSelectSession.mockReset();
    mockPush.mockReset();
    mockPathname = "/chat/";
    window.localStorage.clear();
    usePaletteStore.setState({ open: true, target: null, highlightedMessageId: null });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  // (а) 1 символ не вызывает API
  it("does not call the API for a 1-character query", async () => {
    render(<SearchPalette />);
    const input = screen.getByPlaceholderText("palette.placeholder");
    fireEvent.change(input, { target: { value: "a" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    expect(mockSearchAll).not.toHaveBeenCalled();
  });

  // (б) 2+ символа после debounce вызывает API
  it("calls the API after debounce once the query reaches 2 characters", async () => {
    render(<SearchPalette />);
    const input = screen.getByPlaceholderText("palette.placeholder");
    fireEvent.change(input, { target: { value: "ab" } });
    // Not yet — debounce hasn't elapsed.
    await act(async () => { await vi.advanceTimersByTimeAsync(100); });
    expect(mockSearchAll).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(200); });
    expect(mockSearchAll).toHaveBeenCalledTimes(1);
    expect(mockSearchAll).toHaveBeenCalledWith("ab", { agent: "Agent1" });
  });

  // (в) секции «Сессии»/«Сообщения» рендерятся
  it("renders sessions and messages sections with results", async () => {
    render(<SearchPalette />);
    const input = screen.getByPlaceholderText("palette.placeholder");
    fireEvent.change(input, { target: { value: "hello" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(300); });
    await flush();

    expect(screen.getByText("palette.sessions")).toBeInTheDocument();
    expect(screen.getByText("palette.messages")).toBeInTheDocument();
    expect(screen.getByText("Session B")).toBeInTheDocument();
    expect(screen.getByText("Session A")).toBeInTheDocument();
    // Snippet is split on <b>/</b> markers and rendered as <mark>, never raw HTML.
    expect(screen.getByText("llo")).toBeInTheDocument();
    expect(screen.getByText("llo").tagName.toLowerCase()).toBe("mark");
  });

  // (г) стрелки/Enter выбирают результат — spy on the handleSelect effect (palette closes)
  it("ArrowDown + Enter select a result, closing the palette", async () => {
    render(<SearchPalette />);
    const input = screen.getByPlaceholderText("palette.placeholder");
    fireEvent.change(input, { target: { value: "hello" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(300); });
    await flush();

    expect(usePaletteStore.getState().open).toBe(true);
    fireEvent.keyDown(window, { key: "ArrowDown" });
    fireEvent.keyDown(window, { key: "Enter" });
    expect(usePaletteStore.getState().open).toBe(false);
  });

  // Task 4 — selecting a message result for the CURRENT agent while already on
  // /chat: navigate in-place via selectSession + set the scroll target (Task 3's
  // use-scroll-to-message consumes it once history lands), no route push.
  // mockPathname is "/chat/" (trailing slash, the production value under
  // trailingSlash:true) — this test guards the pathname normalization.
  it("message result, same agent, on /chat/: selectSession + setTarget, no router.push", async () => {
    render(<SearchPalette />);
    const input = screen.getByPlaceholderText("palette.placeholder");
    fireEvent.change(input, { target: { value: "hello" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(300); });
    await flush();

    const row = screen.getByText("Session A").closest("button");
    expect(row).not.toBeNull();
    fireEvent.mouseDown(row!);

    expect(mockSelectSession).toHaveBeenCalledWith("s1", "Agent1");
    expect(mockPush).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().target).toEqual({ sessionId: "s1", messageId: "m1" });
    expect(usePaletteStore.getState().open).toBe(false);
  });

  // Selecting a message result belonging to a DIFFERENT agent must cross-agent
  // jump via router.push (encoded agent + session query params) — selectSession
  // would silently reuse the wrong agent's session pool.
  it("message result, other agent: router.push with encoded agent+session, setTarget still set", async () => {
    mockSearchAll.mockResolvedValue({
      sessions: [],
      messages: [{ ...SEARCH_RESULT.messages[0], agent_id: "Agent 2" }],
      count: 1,
    });
    render(<SearchPalette />);
    const input = screen.getByPlaceholderText("palette.placeholder");
    fireEvent.change(input, { target: { value: "hello" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(300); });
    await flush();

    const row = screen.getByText("Session A").closest("button");
    fireEvent.mouseDown(row!);

    expect(mockPush).toHaveBeenCalledWith("/chat?agent=Agent%202&s=s1");
    expect(mockSelectSession).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().target).toEqual({ sessionId: "s1", messageId: "m1" });
    expect(usePaletteStore.getState().open).toBe(false);
  });

  // Same agent but the palette was opened from a NON-chat page — there is no
  // mounted chat page for selectSession to act on, so it must route to /chat.
  it("message result, same agent, non-chat page: router.push, not selectSession", async () => {
    mockPathname = "/agents/";
    render(<SearchPalette />);
    const input = screen.getByPlaceholderText("palette.placeholder");
    fireEvent.change(input, { target: { value: "hello" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(300); });
    await flush();

    const row = screen.getByText("Session A").closest("button");
    fireEvent.mouseDown(row!);

    expect(mockPush).toHaveBeenCalledWith("/chat?agent=Agent1&s=s1");
    expect(mockSelectSession).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().target).toEqual({ sessionId: "s1", messageId: "m1" });
    expect(usePaletteStore.getState().open).toBe(false);
  });

  // Session-result rows never set a scroll target — there's no specific
  // message to jump to, just the session itself.
  it("session result: selectSession without setTarget", async () => {
    render(<SearchPalette />);
    const input = screen.getByPlaceholderText("palette.placeholder");
    fireEvent.change(input, { target: { value: "hello" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(300); });
    await flush();

    const row = screen.getByText("Session B").closest("button");
    fireEvent.mouseDown(row!);

    expect(mockSelectSession).toHaveBeenCalledWith("s2", "Agent1");
    expect(mockPush).not.toHaveBeenCalled();
    expect(usePaletteStore.getState().target).toBeNull();
    expect(usePaletteStore.getState().open).toBe(false);
  });

  // (д) тогл «по всем» перезапрашивает с all=true и рендерит бейджи агентов у ВСЕХ строк
  it("toggling all-agents re-queries with all=true and renders an agent badge on every row", async () => {
    render(<SearchPalette />);
    const input = screen.getByPlaceholderText("palette.placeholder");
    fireEvent.change(input, { target: { value: "hello" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(300); });
    await flush();

    expect(mockSearchAll).toHaveBeenLastCalledWith("hello", { agent: "Agent1" });
    expect(screen.queryAllByText("Agent1")).toHaveLength(0);

    const toggle = screen.getByRole("switch");
    fireEvent.click(toggle);
    await flush();

    expect(mockSearchAll).toHaveBeenLastCalledWith("hello", { all: true });
    // One badge per result row (1 session + 1 message).
    expect(screen.getAllByText("Agent1")).toHaveLength(2);
  });

  // (е) состояние тогла персистится (localStorage)
  it("persists the all-agents toggle to localStorage and hydrates it on next mount", async () => {
    const { unmount } = render(<SearchPalette />);
    const toggle = screen.getByRole("switch");
    expect(toggle).toHaveAttribute("aria-checked", "false");
    fireEvent.click(toggle);
    expect(window.localStorage.getItem("palette_all_agents")).toBe("1");
    unmount();

    render(<SearchPalette />);
    const toggle2 = screen.getByRole("switch");
    expect(toggle2).toHaveAttribute("aria-checked", "true");
  });

  // I2: opening the palette clears any stale pending jump target — a dangling
  // target (failed nav / never-reached jump) would otherwise suppress silent
  // scroll-restores and could fire a surprise delayed jump later.
  it("clears a stale pending jump target when the palette opens", async () => {
    usePaletteStore.setState({
      open: false,
      target: { sessionId: "s9", messageId: "m9" },
      highlightedMessageId: null,
    });
    render(<SearchPalette />);
    // Palette closed on mount → target untouched.
    expect(usePaletteStore.getState().target).toEqual({ sessionId: "s9", messageId: "m9" });

    await act(async () => {
      usePaletteStore.setState({ open: true });
      await Promise.resolve();
    });
    expect(usePaletteStore.getState().target).toBeNull();
  });

  // (ж) Ctrl+K opens the palette even while a textarea elsewhere has focus —
  // the palette owns Ctrl+K globally (allowInInput), it must not be
  // swallowed by the composer's own input handling.
  it("Ctrl+K opens the palette even when a textarea has focus", async () => {
    usePaletteStore.setState({ open: false, target: null, highlightedMessageId: null });
    render(
      <>
        <textarea data-testid="composer" />
        <SearchPalette />
      </>,
    );
    const textarea = screen.getByTestId("composer");
    textarea.focus();
    expect(document.activeElement).toBe(textarea);

    fireEvent.keyDown(textarea, { key: "k", ctrlKey: true });
    expect(usePaletteStore.getState().open).toBe(true);
  });
});
