import { vi, describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// ── Polyfill: ResizeObserver (not available in jsdom) ──────────────────────

globalThis.ResizeObserver = class ResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof globalThis.ResizeObserver;

// ── Polyfill: IntersectionObserver (not available in jsdom) ─────────────────

globalThis.IntersectionObserver = class IntersectionObserver {
  constructor() {}
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof globalThis.IntersectionObserver;

// ── Polyfill: Element.scrollIntoView (not available in jsdom) ───────────────

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

// ── Mock: use-tool-progress ────────────────────────────────────────────────

vi.mock("@/hooks/use-tool-progress", () => ({
  useToolProgress: () => 0,
}));

// ── Mock: stores ───────────────────────────────────────────────────────────

const mockChatStoreState: Record<string, unknown> = {
  currentAgent: "TestAgent",
  agents: {
    TestAgent: {
      activeSessionId: null,
      activeSessionIds: [],
      messageSource: { mode: "new-chat" },
      streamError: null,
      inputText: "",
    },
  },
};

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = {
        token: "test-token",
        isAuthenticated: true,
        version: "1.0.0",
        agents: ["TestAgent", "Agent1", "Helper", "HistoryAgent", "DirectAgent", "SenderAgent"],
        agentIcons: { Agent1: "agent1-icon.png", Helper: "helper-icon.png" },
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
      return selector ? selector(mockChatStoreState) : mockChatStoreState;
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

// ── Mock: markdown rendering ───────────────────────────────────────────────

vi.mock("@/components/ui/markdown", () => ({
  Markdown: ({ children }: { children: string }) => <div data-testid="markdown">{children}</div>,
}));

// ── Mock: rich-card ────────────────────────────────────────────────────────

vi.mock("@/components/ui/rich-card", () => ({
  TableCard: ({ data }: { data: unknown }) => <div data-testid="table-card">{JSON.stringify(data)}</div>,
  MetricCard: ({ data }: { data: unknown }) => <div data-testid="metric-card">{JSON.stringify(data)}</div>,
}));

// ── Import components under test ───────────────────────────────────────────

import { MessageList } from "@/app/(authenticated)/chat/MessageList";
import { MessageItem } from "@/app/(authenticated)/chat/MessageItem";
import type { ChatMessage } from "@/stores/chat-store";

// ── Helper: build messages ─────────────────────────────────────────────────

function makeMsg(overrides: Partial<ChatMessage> & { id: string; role: ChatMessage["role"] }): ChatMessage {
  return {
    parts: [{ type: "text", text: "Hello" }],
    ...overrides,
  };
}

// ── Tests ──────────────────────────────────────────────────────────────────

describe("Multi-Agent Identity (MAID)", () => {
  // MAID-01: Agent turn separator between consecutive different-agent assistant messages
  describe("MAID-01: Agent turn separators", () => {
    it("renders separator between consecutive assistant messages from different agents — Phase 15 VSEP-01", () => {
      const messages: ChatMessage[] = [
        makeMsg({ id: "1", role: "assistant", agentId: "Agent1", parts: [{ type: "text", text: "I am Agent1" }] }),
        makeMsg({ id: "2", role: "assistant", agentId: "Helper", parts: [{ type: "text", text: "I am Helper" }] }),
      ];

      render(
        <MessageList
          messages={messages}
          isStreaming={false}
          showThinking={false}
          isLoadingHistory={false}
          emptyState={<div />}
          hiddenCount={0}
          onLoadEarlier={() => {}}
        />,
      );

      const separator = screen.getByRole("separator");
      expect(separator).toBeInTheDocument();
    });

    it("does NOT render separator between consecutive assistant messages from the SAME agent", () => {
      const messages: ChatMessage[] = [
        makeMsg({ id: "1", role: "assistant", agentId: "Agent1", parts: [{ type: "text", text: "First" }] }),
        makeMsg({ id: "2", role: "assistant", agentId: "Agent1", parts: [{ type: "text", text: "Second" }] }),
      ];

      render(
        <MessageList
          messages={messages}
          isStreaming={false}
          showThinking={false}
          isLoadingHistory={false}
          emptyState={<div />}
          hiddenCount={0}
          onLoadEarlier={() => {}}
        />,
      );

      expect(screen.queryByRole("separator")).not.toBeInTheDocument();
    });

    it("does NOT render separator when user message sits between different-agent assistants", () => {
      const messages: ChatMessage[] = [
        makeMsg({ id: "1", role: "assistant", agentId: "Agent1", parts: [{ type: "text", text: "Agent1 says" }] }),
        makeMsg({ id: "2", role: "user", parts: [{ type: "text", text: "User question" }] }),
        makeMsg({ id: "3", role: "assistant", agentId: "Helper", parts: [{ type: "text", text: "Helper says" }] }),
      ];

      render(
        <MessageList
          messages={messages}
          isStreaming={false}
          showThinking={false}
          isLoadingHistory={false}
          emptyState={<div />}
          hiddenCount={0}
          onLoadEarlier={() => {}}
        />,
      );

      expect(screen.queryByRole("separator")).not.toBeInTheDocument();
    });
  });

  // MAID-02: ThinkingMessage shows animation only (no agent name/avatar)
  describe("MAID-02: ThinkingMessage agent display", () => {
    it("renders ThinkingMessage without agent name (animation only)", () => {
      render(
        <MessageList
          messages={[]}
          isStreaming={true}
          showThinking={true}
          isLoadingHistory={false}
          emptyState={<div />}
          hiddenCount={0}
          onLoadEarlier={() => {}}
        />,
      );

      // Agent name is not shown in ThinkingMessage — only the animation indicator
      expect(screen.queryByText("TestAgent")).not.toBeInTheDocument();
    });
  });

  // MAID-03: History messages show correct agent avatars from agentId in DB
  describe("MAID-03: History message agent identity", () => {
    it("renders assistant message with agentId from message prop (not from store)", () => {
      const msg = makeMsg({
        id: "hist-1",
        role: "assistant",
        agentId: "HistoryAgent",
        parts: [{ type: "text", text: "Historical reply" }],
      });

      render(<MessageItem message={msg} />);
      expect(screen.getByText("HistoryAgent")).toBeInTheDocument();
    });
  });

  // STATE-03: Agent avatar stable after forward-fill
  describe("STATE-03: agent avatar stable with forward-filled agentId", () => {
    it("STATE-03: renders correct agent name for message with forward-filled agentId", () => {
      // Simulates a message where agentId was forward-filled from a prior DB row
      // (i.e., DB had null agent_id, convertHistory filled it from the previous non-null row)
      const msg = makeMsg({
        id: "ff-1",
        role: "assistant",
        agentId: "Agent1",
        parts: [{ type: "text", text: "Reply from forward-filled agent" }],
      });

      render(<MessageItem message={msg} />);
      // Agent name should be visible, proving forward-filled agentId is respected
      expect(screen.getByText("Agent1")).toBeInTheDocument();
    });
  });

  // MAID-04: No assistant-ui hooks for agent identity
  describe("MAID-04: No assistant-ui identity dependency", () => {
    it("MessageItem uses agentId from message prop directly without assistant-ui hooks", () => {
      // This test verifies the implementation approach: agentId comes from ChatMessage.agentId
      // not from useMessage() or useMessageRuntime() or any assistant-ui context
      const msg = makeMsg({
        id: "direct-1",
        role: "assistant",
        agentId: "DirectAgent",
        parts: [{ type: "text", text: "Direct agent reply" }],
      });

      render(<MessageItem message={msg} />);
      // Agent name appears -- proving it comes from prop, not assistant-ui context
      expect(screen.getByText("DirectAgent")).toBeInTheDocument();
    });
  });

  // AGENT-01/AGENT-02: stable identity + visual distinction
  describe("AGENT-01/AGENT-02 — stable identity + visual distinction", () => {
    // AGENT-01a: first SSE assistant message has agentId from primary agent
    it("AGENT-01a: assistant message agentId equals primary agent", () => {
      // currentRespondingAgent falls back to the primary agent name (not null/undefined).
      // This test verifies the resulting message renders the correct agent name.
      const msg = makeMsg({
        id: "sse-1",
        role: "assistant",
        agentId: "TestAgent", // fix sets currentRespondingAgent ?? agent (primary agent name)
        parts: [{ type: "text", text: "SSE reply from primary agent" }],
      });

      render(<MessageItem message={msg} />);
      // Agent name "TestAgent" must appear — confirms agentId is set from primary agent name
      expect(screen.getByText("TestAgent")).toBeInTheDocument();
    });

    // AGENT-02: inter-agent sender message renders with distinct visual treatment
    it("AGENT-02: inter-agent sender message renders with data-role=agent-sender and bg-muted/20", () => {
      // A user-role message with agentId set means it's an inter-agent message.
      // It must be visually distinct from regular user messages.
      const agentSenderMsg = makeMsg({
        id: "inter-1",
        role: "user",
        agentId: "SenderAgent", // inter-agent message: user role but sent by another agent
        parts: [{ type: "text", text: "Inter-agent message from SenderAgent" }],
      });

      const { container } = render(<MessageItem message={agentSenderMsg} />);

      // Wrapper element must identify as agent-sender, not generic user
      const wrapper = container.querySelector("[data-role='agent-sender']");
      expect(wrapper).toBeInTheDocument();

      // Must have the bg-muted/20 background class for visual distinction — agent-sender.*bg-muted
      expect(wrapper!.className).toMatch(/bg-muted\/20/);
    });
  });

  // WS6: participant hygiene — a session recreated behind the scenes must
  // never leak its raw UUID (or any id that isn't a configured agent) to
  // the visible chat UI as a participant label.
  describe("WS6: unknown/UUID agentId renders a generic label, never the raw id", () => {
    const SESSION_UUID = "9f8b6c1a-2d3e-4f5a-8b7c-1a2b3c4d5e6f";

    it("assistant message with a UUID-shaped agentId renders the generic label, not the UUID", () => {
      const msg = makeMsg({
        id: "ws6-assistant-uuid",
        role: "assistant",
        agentId: SESSION_UUID,
        parts: [{ type: "text", text: "Reply after silent session recreation" }],
      });

      render(<MessageItem message={msg} />);
      expect(screen.getByText("chat.unknown_agent")).toBeInTheDocument();
      expect(screen.queryByText(SESSION_UUID)).not.toBeInTheDocument();
    });

    it("agent-sender (inter-agent) message with a UUID-shaped agentId renders the generic label", () => {
      const msg = makeMsg({
        id: "ws6-sender-uuid",
        role: "user",
        agentId: SESSION_UUID,
        parts: [{ type: "text", text: "Inter-agent message with a bogus id" }],
      });

      render(<MessageItem message={msg} />);
      expect(screen.getByText("chat.unknown_agent")).toBeInTheDocument();
      expect(screen.queryByText(SESSION_UUID)).not.toBeInTheDocument();
    });

    it("assistant message whose agentId is not in the known-agents list renders the generic label", () => {
      const msg = makeMsg({
        id: "ws6-not-known",
        role: "assistant",
        agentId: "GhostAgentThatWasDeleted",
        parts: [{ type: "text", text: "Reply from a deleted/unknown agent" }],
      });

      render(<MessageItem message={msg} />);
      expect(screen.getByText("chat.unknown_agent")).toBeInTheDocument();
      expect(screen.queryByText("GhostAgentThatWasDeleted")).not.toBeInTheDocument();
    });

    it("a real, known agent name still renders unchanged (no regression)", () => {
      const msg = makeMsg({
        id: "ws6-known",
        role: "assistant",
        agentId: "Agent1",
        parts: [{ type: "text", text: "Reply from a real agent" }],
      });

      render(<MessageItem message={msg} />);
      expect(screen.getByText("Agent1")).toBeInTheDocument();
      expect(screen.queryByText("chat.unknown_agent")).not.toBeInTheDocument();
    });

    it("AgentTransitionDivider renders the generic label instead of a raw UUID agentId", () => {
      const messages: ChatMessage[] = [
        makeMsg({ id: "1", role: "assistant", agentId: "Agent1", parts: [{ type: "text", text: "I am Agent1" }] }),
        makeMsg({ id: "2", role: "assistant", agentId: SESSION_UUID, parts: [{ type: "text", text: "Recreated-session reply" }] }),
      ];

      render(
        <MessageList
          messages={messages}
          isStreaming={false}
          showThinking={false}
          isLoadingHistory={false}
          emptyState={<div />}
          hiddenCount={0}
          onLoadEarlier={() => {}}
        />,
      );

      const separator = screen.getByRole("separator");
      expect(separator).toBeInTheDocument();
      expect(separator.textContent).not.toContain(SESSION_UUID);
      expect(separator.textContent).toContain("chat.unknown_agent");
    });
  });
});
