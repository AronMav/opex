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

// ── Mock: chat-store (only currentAgent is read by the palette) ────────────

vi.mock("@/stores/chat-store", () => ({
  useChatStore: (selector: (s: { currentAgent: string }) => unknown) =>
    selector({ currentAgent: "Agent1" }),
}));

// ── Mock: search-api ─────────────────────────────────────────────────────────

const mockSearchAll = vi.fn();
vi.mock("@/lib/search-api", () => ({
  searchAll: (...args: unknown[]) => mockSearchAll(...args),
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
});
