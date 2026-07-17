import { vi, describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// ── Polyfill: ResizeObserver (not available in jsdom) ──────────────────────

globalThis.ResizeObserver = class ResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof globalThis.ResizeObserver;

globalThis.IntersectionObserver = class IntersectionObserver {
  constructor() {}
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof globalThis.IntersectionObserver;

// scrollIntoView is not available in jsdom
Element.prototype.scrollIntoView = vi.fn();

// ── Mock: next/navigation ──────────────────────────────────────────────────

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));

// ── Mock: sonner toast ─────────────────────────────────────────────────────

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

// ── Mock: translation hook ─────────────────────────────────────────────────

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({
    t: (key: string) => key,
    locale: "en",
  }),
}));

// ── Mock: stores ───────────────────────────────────────────────────────────

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = {
        token: "test-token",
        isAuthenticated: true,
        version: "1.0.0",
        agents: ["TestAgent"],
        agentIcons: {},
        lastFetched: Date.now(),
        login: vi.fn(),
        logout: vi.fn(),
        restore: vi.fn(),
        refreshIfStale: vi.fn(),
      };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token", logout: vi.fn() }) },
  ),
}));

vi.mock("@/stores/chat-store", () => ({
  useChatStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const agentState = {
        activeSessionId: null,
        activeSessionIds: [],
        messageSource: { mode: "new-chat" },
        streamError: null,
        inputText: "",
      };
      const state: Record<string, unknown> = {
        currentAgent: "TestAgent",
        agents: { TestAgent: agentState },
      };
      return selector ? selector(state) : state;
    },
    {
      getState: () => ({
        currentAgent: "TestAgent",
        agents: { TestAgent: { activeSessionId: null, activeSessionIds: [], messageSource: { mode: "new-chat" }, connectionPhase: "idle" } },
        regenerate: vi.fn(),
        clearError: vi.fn(),
        sendMessage: vi.fn(),
        deleteMessage: vi.fn().mockResolvedValue(undefined),
        editMessage: vi.fn(),
        exportSession: vi.fn(),
      }),
    },
  ),
  convertHistory: () => [],
  MAX_INPUT_LENGTH: 32000,
}));

// ── Mock: @/lib/queries ────────────────────────────────────────────────────

vi.mock("@/lib/queries", () => ({
  useSessions: () => ({ sessions: [], total: 0, isLoading: false, isFetched: true, fetchNextPage: vi.fn(), hasNextPage: false, isFetchingNextPage: false }),
  useSessionMessages: () => ({ data: { messages: [] }, isLoading: false, error: null, refetch: vi.fn() }),
  useAgents: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
  useProviders: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
  useProviderModels: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
  useProviderActive: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
}));

// ── Mock: @/hooks/use-profiles — ReloadButton's model-picker source (13a).
// Stubbed wholesale (no models) so MessageActions' showReload branch doesn't
// need a real QueryClientProvider; these tests don't exercise regenerate.
vi.mock("@/hooks/use-profiles", () => ({
  useAgentModelOptions: () => ({ models: [], defaultModel: "" }),
}));

// ── Mock: @/lib/sanitize-url ───────────────────────────────────────────────

vi.mock("@/lib/sanitize-url", () => ({
  sanitizeUrl: (url: string) => url,
}));

// ── Mock: @/lib/api ────────────────────────────────────────────────────────

vi.mock("@/lib/api", () => ({
  apiGet: vi.fn().mockResolvedValue({}),
  apiPost: vi.fn().mockResolvedValue({}),
  apiPut: vi.fn().mockResolvedValue({}),
  apiDelete: vi.fn().mockResolvedValue(undefined),
  getToken: () => "test-token",
  assertToken: () => "test-token",
}));

// ── Mock: @/lib/query-client ───────────────────────────────────────────────

vi.mock("@/lib/query-client", () => ({
  queryClient: { invalidateQueries: vi.fn(), setQueryData: vi.fn() },
}));

// ── Mock: @tanstack/react-query ────────────────────────────────────────────

vi.mock("@tanstack/react-query", async () => {
  const actual = await vi.importActual("@tanstack/react-query");
  return {
    ...actual,
    useQueryClient: () => ({ invalidateQueries: vi.fn(), setQueryData: vi.fn() }),
    useQuery: () => ({ data: undefined, isLoading: false, error: null, refetch: vi.fn() }),
  };
});

// ── Mock: zustand/react/shallow ────────────────────────────────────────────

vi.mock("zustand/react/shallow", () => ({
  useShallow: (fn: unknown) => fn,
}));

// ── Mock: react-virtuoso (no real layout in jsdom) ────────────────────────

vi.mock("react-virtuoso", () => {
  const React = require("react");
  return {
    Virtuoso: React.forwardRef(function MockVirtuoso(props: Record<string, unknown>, ref: unknown) {
      const divRef = React.useRef(null);
      React.useImperativeHandle(ref, () => ({
        scrollToIndex: () => {},
        scrollTo: () => {},
        scrollBy: () => {},
        scrollIntoView: () => {},
      }));
      const data = (props.data ?? []) as unknown[];
      const itemContent = props.itemContent as ((index: number, item: unknown) => React.ReactNode) | undefined;
      const components = props.components as {
        Header?: () => React.ReactNode;
        Footer?: () => React.ReactNode;
        List?: React.ComponentType<Record<string, unknown>>;
        Item?: React.ComponentType<Record<string, unknown>>;
      } | undefined;
      const Item = components?.Item;
      const List = components?.List;
      const itemEls = data.map((item: unknown, i: number) => {
        const content = itemContent ? itemContent(i, item) : null;
        return Item
          ? React.createElement(Item, { key: i, "data-index": i }, content)
          : React.createElement("div", { key: i }, content);
      });
      const body = List ? React.createElement(List, {}, itemEls) : itemEls;
      return React.createElement("div", { "data-testid": "virtuoso-mock", ref: divRef },
        components?.Header ? React.createElement(components.Header) : null,
        body,
        components?.Footer ? React.createElement(components.Footer) : null,
      );
    }),
  };
});

// ── Mock: markdown rendering (simplify for tests) ──────────────────────────

vi.mock("@/components/ui/markdown", () => ({
  Markdown: ({ children }: { children: string }) => <div data-testid="markdown">{children}</div>,
}));

// ── Mock: rich-card ────────────────────────────────────────────────────────

vi.mock("@/components/ui/rich-card", () => ({
  TableCard: ({ data }: { data: unknown }) => <div data-testid="table-card">{JSON.stringify(data)}</div>,
  MetricCard: ({ data }: { data: unknown }) => <div data-testid="metric-card">{JSON.stringify(data)}</div>,
}));

// ── Import components under test ───────────────────────────────────────────

import { MessageItem } from "@/app/(authenticated)/chat/MessageItem";
import { MessageList } from "@/app/(authenticated)/chat/MessageList";
import type { ChatMessage } from "@/stores/chat-store";

// ── Tests ──────────────────────────────────────────────────────────────────

describe("MessageItem", () => {
  it("renders user message with 'You' label (REND-03)", () => {
    const msg: ChatMessage = {
      id: "1",
      role: "user",
      parts: [{ type: "text", text: "Hello world" }],
    };
    render(<MessageItem message={msg} />);
    expect(screen.getByText("chat.you")).toBeInTheDocument();
  });

  it("renders assistant message with agentId name (REND-02)", () => {
    const msg: ChatMessage = {
      id: "2",
      role: "assistant",
      parts: [{ type: "text", text: "Hi there" }],
      agentId: "Agent1",
    };
    render(<MessageItem message={msg} />);
    expect(screen.getByText("Agent1")).toBeInTheDocument();
  });

  it("renders text part with markdown content (REND-07)", () => {
    const msg: ChatMessage = {
      id: "3",
      role: "assistant",
      parts: [{ type: "text", text: "**bold text**" }],
      agentId: "Bot",
    };
    render(<MessageItem message={msg} />);
    // Text passes through cleanContent then to MessageContent with markdown
    expect(screen.getByText("**bold text**")).toBeInTheDocument();
  });

  it("renders tool call part with tool name (REND-05)", () => {
    const msg: ChatMessage = {
      id: "4",
      role: "assistant",
      parts: [
        {
          type: "tool",
          toolCallId: "tc1",
          toolName: "web_search",
          state: "output-available" as const,
          input: { query: "test" },
          output: "search results",
        },
      ],
      agentId: "Bot",
    };
    render(<MessageItem message={msg} />);
    expect(screen.getByText("web_search")).toBeInTheDocument();
  });

  it("renders file part with audio element (REND-08)", () => {
    const msg: ChatMessage = {
      id: "6",
      role: "assistant",
      parts: [{ type: "file", url: "/uploads/test.mp3", mediaType: "audio/mpeg" }],
      agentId: "Bot",
    };
    const { container } = render(<MessageItem message={msg} />);
    const audio = container.querySelector("audio");
    expect(audio).toBeInTheDocument();
    expect(audio?.getAttribute("src")).toBe("/uploads/test.mp3");
  });

  it("shows loading indicator for empty assistant parts", () => {
    const msg: ChatMessage = {
      id: "7",
      role: "assistant",
      parts: [],
      agentId: "Bot",
    };
    const { getByTestId, container } = render(<MessageItem message={msg} />);
    // W4-3: empty parts render PartSkeleton (quiet skeleton bar), not CometLoader —
    // the sr-only "Loading" label lives inside the skeleton.
    expect(getByTestId("part-skeleton")).toBeInTheDocument();
    const liveLabel = container.querySelector('[class*="sr-only"]');
    expect(liveLabel).toBeInTheDocument();
  });

  // W4-3 fix: the inline caret means "text is arriving RIGHT HERE" — a
  // streaming message must show at most ONE caret, and only when its LAST
  // part is a text part (interleaved tool turns show the tool chip's own
  // running indicator instead).
  it("streaming message with [text, tool, text] renders exactly one caret", () => {
    const msg: ChatMessage = {
      id: "w43-multi",
      role: "assistant",
      status: "streaming",
      parts: [
        { type: "text", text: "first chunk" },
        {
          type: "tool",
          toolCallId: "tc-w43",
          toolName: "web_search",
          state: "output-available" as const,
          input: { query: "test" },
          output: "results",
        },
        { type: "text", text: "second chunk" },
      ],
      agentId: "Bot",
    };
    const { queryAllByTestId } = render(<MessageItem message={msg} />);
    expect(queryAllByTestId("streaming-cursor")).toHaveLength(1);
  });

  it("streaming message whose last part is a tool renders zero carets", () => {
    const msg: ChatMessage = {
      id: "w43-tool-last",
      role: "assistant",
      status: "streaming",
      parts: [
        { type: "text", text: "some text" },
        {
          type: "tool",
          toolCallId: "tc-w43b",
          toolName: "web_search",
          state: "input-available" as const,
          input: { query: "test" },
        },
      ],
      agentId: "Bot",
    };
    const { queryAllByTestId } = render(<MessageItem message={msg} />);
    expect(queryAllByTestId("streaming-cursor")).toHaveLength(0);
  });

  it("renders inter-agent user message with agent sender name (REND-03)", () => {
    const msg: ChatMessage = {
      id: "8",
      role: "user",
      parts: [{ type: "text", text: "Delegated task" }],
      agentId: "Helper",
    };
    render(<MessageItem message={msg} />);
    expect(screen.getByText("Helper")).toBeInTheDocument();
  });
});

// ── Turn animations ───────────────────────────────────────────────────────

describe("Turn animations", () => {
  it("new message (createdAt = now) renders with animate-in class (ANIM-01)", () => {
    const msg: ChatMessage = {
      id: "anim-1",
      role: "assistant",
      parts: [{ type: "text", text: "Fresh message" }],
      agentId: "Bot",
      createdAt: new Date().toISOString(),
    };
    const { container } = render(
      <MessageList
        messages={[msg]}
        isStreaming={false}
        showThinking={false}
        isLoadingHistory={false}
        emptyState={null}
        hiddenCount={0}
        onLoadEarlier={() => {}}
      />,
    );
    expect(container.querySelector(".animate-in")).toBeInTheDocument();
  });

  it("old message (createdAt = 10s ago) renders WITHOUT animate-in class (ANIM-03)", () => {
    const msg: ChatMessage = {
      id: "anim-2",
      role: "assistant",
      parts: [{ type: "text", text: "Old message" }],
      agentId: "Bot",
      createdAt: new Date(Date.now() - 10000).toISOString(),
    };
    const { container } = render(
      <MessageList
        messages={[msg]}
        isStreaming={false}
        showThinking={false}
        isLoadingHistory={false}
        emptyState={null}
        hiddenCount={0}
        onLoadEarlier={() => {}}
      />,
    );
    expect(container.querySelector(".animate-in")).not.toBeInTheDocument();
  });

  it("message with no createdAt renders WITHOUT animate-in class (ANIM-03)", () => {
    const msg: ChatMessage = {
      id: "anim-3",
      role: "assistant",
      parts: [{ type: "text", text: "No timestamp" }],
      agentId: "Bot",
    };
    const { container } = render(
      <MessageList
        messages={[msg]}
        isStreaming={false}
        showThinking={false}
        isLoadingHistory={false}
        emptyState={null}
        hiddenCount={0}
        onLoadEarlier={() => {}}
      />,
    );
    expect(container.querySelector(".animate-in")).not.toBeInTheDocument();
  });

});

// ── Live region + list semantics (C1 + H1) ────────────────────────────────

describe("MessageList a11y (C1 + H1)", () => {
  const baseMsg: ChatMessage = {
    id: "a11y-1",
    role: "assistant",
    parts: [{ type: "text", text: "hi" }],
    agentId: "Bot",
    createdAt: new Date(Date.now() - 10000).toISOString(),
  };
  function renderList() {
    return render(
      <MessageList
        messages={[baseMsg]}
        isStreaming={false}
        showThinking={false}
        isLoadingHistory={false}
        emptyState={null}
        hiddenCount={0}
        onLoadEarlier={() => {}}
      />,
    );
  }
  it("keeps the thread a log landmark but delegates streaming announcements (C1)", () => {
    renderList();
    const log = screen.getByRole("log");
    // The dedicated StreamingAnnouncer now owns streaming announcements, so the
    // thread-level log is no longer a live region (avoids additions-only gaps
    // and nested-live-region double-announce).
    expect(log).toHaveAttribute("aria-live", "off");
    expect(log).toHaveAccessibleName("chat.message_thread");
  });
  it("exposes each message row as a listitem (H1)", () => {
    renderList();
    expect(screen.getAllByRole("listitem").length).toBeGreaterThan(0);
  });
});

// ── Double-fetch guard (B3) ────────────────────────────────────────────────

describe("MessageList load-earlier guard (B3)", () => {
  it("calls onLoadEarlier only once when the button is clicked twice before data arrives", () => {
    const onLoadEarlier = vi.fn();
    render(
      <MessageList
        messages={[{
          id: "m1", role: "assistant", parts: [{ type: "text", text: "hi" }],
          agentId: "Bot", createdAt: new Date(Date.now() - 10000).toISOString(),
        }]}
        isStreaming={false}
        showThinking={false}
        isLoadingHistory={false}
        emptyState={null}
        hiddenCount={5}
        onLoadEarlier={onLoadEarlier}
      />,
    );
    const btn = screen.getByRole("button", { name: "chat.show_earlier" });
    fireEvent.click(btn);
    fireEvent.click(btn);
    expect(onLoadEarlier).toHaveBeenCalledTimes(1);
  });
});

// ── Thinking indicator live region (H3) ───────────────────────────────────

describe("MessageList thinking indicator (H3)", () => {
  it("announces the thinking state via role=status", () => {
    render(
      <MessageList
        messages={[]}
        isStreaming={true}
        showThinking={true}
        isLoadingHistory={false}
        emptyState={null}
        hiddenCount={0}
        onLoadEarlier={() => {}}
      />,
    );
    const status = screen.getByRole("status");
    expect(status).toBeInTheDocument();
    expect(status).toHaveAttribute("aria-live", "polite");
  });
});

// ── Virtualization stress (UI-04) ─────────────────────────────────────────

function generateToolMessages(count: number): ChatMessage[] {
  const OLD_TIMESTAMP = new Date(Date.now() - 10 * 60 * 1000).toISOString(); // 10 min ago — no animation
  const msgs: ChatMessage[] = [];
  for (let i = 0; i < count; i++) {
    if (i % 5 === 0) {
      // Every 5th message is a user message
      msgs.push({
        id: `stress-${i}`,
        role: "user",
        parts: [{ type: "text", text: `User message ${i}` }],
        createdAt: OLD_TIMESTAMP,
      });
    } else {
      // Assistant message with 1–3 tool parts
      const toolCount = (i % 3) + 1;
      const parts: ChatMessage["parts"] = [];
      for (let t = 0; t < toolCount; t++) {
        parts.push({
          type: "tool",
          toolCallId: `tc-${i}-${t}`,
          toolName: "web_search",
          state: "output-available" as const,
          input: { query: "test" },
          output: "result",
        });
      }
      // Every 10th assistant message also gets a rich-card part
      if (i % 10 === 3) {
        parts.push({
          type: "rich-card",
          cardType: "metric",
          data: { label: `Metric${i}`, value: i },
        });
      }
      msgs.push({
        id: `stress-${i}`,
        role: "assistant",
        parts,
        agentId: "TestAgent",
        createdAt: OLD_TIMESTAMP,
      });
    }
  }
  return msgs;
}

describe("Virtualization stress (UI-04)", () => {
  it("renders 200 messages with tool calls without crashing (UI-04)", () => {
    const msgs = generateToolMessages(200);
    render(
      <MessageList
        messages={msgs}
        isStreaming={false}
        showThinking={false}
        isLoadingHistory={false}
        emptyState={null}
        hiddenCount={0}
        onLoadEarlier={() => {}}
      />,
    );
    expect(screen.getByTestId("virtuoso-mock")).toBeInTheDocument();
    expect(screen.getAllByText("web_search").length).toBeGreaterThan(0);
  });

  it("renders 500 messages without timeout (UI-04)", { timeout: 30000 }, () => {
    const msgs = generateToolMessages(500);
    const start = performance.now();
    render(
      <MessageList
        messages={msgs}
        isStreaming={false}
        showThinking={false}
        isLoadingHistory={false}
        emptyState={null}
        hiddenCount={0}
        onLoadEarlier={() => {}}
      />,
    );
    const elapsed = performance.now() - start;
    expect(elapsed).toBeLessThan(15000);
    expect(screen.getByTestId("virtuoso-mock")).toBeInTheDocument();
  });
});
