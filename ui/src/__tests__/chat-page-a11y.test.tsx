import { vi, describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// M2 + M3: the chat page must expose the session list with list/listitem
// semantics and carry a top-level <h1> for landmark navigation.

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));

vi.mock("next/dynamic", () => ({
  default: () => {
    const Stub = () => null;
    Stub.displayName = "DynamicStub";
    return Stub;
  },
}));

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() }),
}));

vi.mock("@/components/ui/sidebar", () => ({
  SidebarProvider: ({ children }: { children: React.ReactNode }) => children,
  SidebarInset: ({ children }: { children: React.ReactNode }) => children,
  SidebarTrigger: () => null,
  useSidebar: () => ({ openMobile: false, setOpenMobile: vi.fn(), isMobile: false, state: "expanded" }),
}));

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

vi.mock("@/hooks/use-ws-subscription", () => ({ useWsSubscription: vi.fn() }));
vi.mock("@/hooks/use-auto-refresh", () => ({ useAutoRefresh: vi.fn() }));
vi.mock("@/hooks/use-tool-progress", () => ({ useToolProgress: () => 0 }));

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = {
        token: "test-token", isAuthenticated: true, version: "1.0.0",
        agents: ["main"], agentIcons: {}, lastFetched: Date.now(),
        login: vi.fn(), logout: vi.fn(), restore: vi.fn(), refreshIfStale: vi.fn(),
      };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token", logout: vi.fn() }) },
  ),
}));

vi.mock("@/stores/ws-store", () => ({
  useWsStore: (selector?: (s: Record<string, unknown>) => unknown) => {
    const state = { ws: null, connected: false, wsStatus: "disconnected", connect: vi.fn(), disconnect: vi.fn() };
    return selector ? selector(state) : state;
  },
}));

vi.mock("@/stores/chat-store", () => ({
  useChatStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const agentState = {
        activeSessionId: null, activeSessionIds: [], messageSource: { mode: "new-chat" },
        streamError: null, messages: [], inputText: "",
      };
      const state: Record<string, unknown> = {
        currentAgent: "main", agents: { main: agentState }, currentSessionId: null,
        messages: [], sessions: [], inputText: "",
      };
      return selector ? selector(state) : state;
    },
    {
      getState: () => ({
        currentAgent: "main",
        agents: { main: { activeSessionId: null, activeSessionIds: [], messageSource: { mode: "new-chat" }, connectionPhase: "idle" } },
        setCurrentAgent: vi.fn(), selectSession: vi.fn(), newChat: vi.fn(),
        deleteSession: vi.fn().mockResolvedValue(undefined),
        deleteAllSessions: vi.fn().mockResolvedValue(undefined),
        renameSession: vi.fn(), regenerate: vi.fn(), clearError: vi.fn(), exportSession: vi.fn(),
      }),
    },
  ),
  isActivePhase: () => false,
  getInitialAgent: (agents: string[]) => agents[0] || "main",
  getLastSessionId: () => undefined,
  MAX_INPUT_LENGTH: 32000,
  convertHistory: () => [],
}));

vi.mock("@/stores/canvas-store", () => ({
  useCanvasStore: (selector?: (s: Record<string, unknown>) => unknown) => {
    const state = { canvases: {}, panelOpen: false, handleEvent: vi.fn(), clearAgent: vi.fn(), togglePanel: vi.fn() };
    return selector ? selector(state) : state;
  },
}));

vi.mock("zustand/react/shallow", () => ({ useShallow: (fn: unknown) => fn }));

vi.mock("@/lib/api", () => ({
  apiGet: vi.fn().mockResolvedValue({}), apiPost: vi.fn().mockResolvedValue({}),
  apiPut: vi.fn().mockResolvedValue({}), apiPatch: vi.fn().mockResolvedValue({}),
  apiDelete: vi.fn().mockResolvedValue(undefined),
  getToken: () => "test-token", assertToken: () => "test-token",
}));

vi.mock("@/lib/ws", () => ({ WsManager: vi.fn() }));

const SESSIONS = [
  { id: "s1", title: "First session", channel: "web", user_id: "u1", run_status: "done", created_at: "", updated_at: "" },
  { id: "s2", title: "Second session", channel: "telegram", user_id: "u2", run_status: "done", created_at: "", updated_at: "" },
];

const emptyQuery = { data: [], isLoading: false, error: null, refetch: vi.fn() };

vi.mock("@/lib/queries", () => ({
  qk: {
    sessions: (a: string) => ["sessions", "list", a],
    sessionMessages: (id: string) => ["sessions", id, "messages"],
  },
  useSessions: () => ({ sessions: SESSIONS, total: SESSIONS.length, isLoading: false, isFetched: true, fetchNextPage: vi.fn(), hasNextPage: false, isFetchingNextPage: false }),
  useAutoPaginateWhileFiltering: () => {},
  useSessionMessages: () => ({ data: { messages: [] }, isLoading: false, error: null, refetch: vi.fn() }),
  useAgents: () => ({ ...emptyQuery, data: [] }),
  useProviderActive: () => ({ ...emptyQuery, data: [] }),
  useProviders: () => ({ ...emptyQuery, data: [] }),
  useProviderModels: () => ({ ...emptyQuery, data: [] }),
  useProviderModelsDetailed: () => ({ ...emptyQuery, data: [] }),
  useAgentTasks: () => ({ ...emptyQuery, data: [] }),
}));

vi.mock("@tanstack/react-query", async () => {
  const actual = await vi.importActual("@tanstack/react-query");
  return {
    ...actual,
    useQueryClient: () => ({ invalidateQueries: vi.fn(), setQueryData: vi.fn() }),
    useQuery: () => ({ data: undefined, isLoading: false, error: null, refetch: vi.fn() }),
  };
});

vi.mock("@/lib/query-client", () => ({
  queryClient: { invalidateQueries: vi.fn(), setQueryData: vi.fn() },
}));

vi.mock("@/providers/assistant-runtime", () => ({
  ChatRuntimeProvider: ({ children }: { children: React.ReactNode }) => children,
}));

// Render Virtuoso items so the session list-items materialise.
vi.mock("react-virtuoso", () => {
  const React = require("react");
  return {
    Virtuoso: (props: Record<string, unknown>) => {
      const data = (props.data ?? []) as unknown[];
      const itemContent = props.itemContent as ((i: number, item: unknown) => React.ReactNode) | undefined;
      const components = props.components as {
        List?: React.ComponentType<Record<string, unknown>>;
        Item?: React.ComponentType<Record<string, unknown>>;
      } | undefined;
      const Item = components?.Item;
      const List = components?.List;
      const itemEls = data.map((item, i) => {
        const content = itemContent ? itemContent(i, item) : null;
        return Item
          ? React.createElement(Item, { key: i }, content)
          : React.createElement("div", { key: i }, content);
      });
      return List
        ? React.createElement(List, { "data-testid": "virtuoso-sessions" }, itemEls)
        : React.createElement("div", { "data-testid": "virtuoso-sessions" }, itemEls);
    },
  };
});

vi.mock("@/lib/cron", () => ({ describeCron: () => "every day", isValidCron: () => true }));

describe("Chat page a11y (M2 + M3)", () => {
  it("renders a top-level heading and a session list with listitems", async () => {
    const { default: Page } = await import("@/app/(authenticated)/chat/page");
    render(<Page />);
    expect(screen.getByRole("heading", { level: 1 })).toBeInTheDocument();
    expect(screen.getByRole("list")).toBeInTheDocument();
    expect(screen.getAllByRole("listitem").length).toBeGreaterThanOrEqual(2);
  });
});
