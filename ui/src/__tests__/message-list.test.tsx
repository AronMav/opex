import { vi, describe, it, expect, beforeAll } from "vitest";
import { render, screen } from "@testing-library/react";
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
  useSessions: () => ({ data: { sessions: [] }, isLoading: false, error: null, refetch: vi.fn() }),
  useSessionMessages: () => ({ data: { messages: [] }, isLoading: false, error: null, refetch: vi.fn() }),
  useAgents: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
  useProviders: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
  useProviderModels: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
  useProviderActive: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
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
      const components = props.components as { Header?: () => React.ReactNode; Footer?: () => React.ReactNode } | undefined;
      return React.createElement("div", { "data-testid": "virtuoso-mock", ref: divRef },
        components?.Header ? React.createElement(components.Header) : null,
        ...(data.map((item: unknown, i: number) =>
          React.createElement("div", { key: i }, itemContent ? itemContent(i, item) : null)
        )),
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
  RichCard: ({ part }: { part: unknown }) => <div data-testid="rich-card">{JSON.stringify(part)}</div>,
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
    const { container } = render(<MessageItem message={msg} />);
    // BarsLoader renders an SVG element
    const svg = container.querySelector("svg");
    expect(svg).toBeInTheDocument();
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

  it("renders 500 messages without timeout (UI-04)", () => {
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
    expect(elapsed).toBeLessThan(5000);
    expect(screen.getByTestId("virtuoso-mock")).toBeInTheDocument();
  });
});
