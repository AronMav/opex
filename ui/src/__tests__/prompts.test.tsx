import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";
import { vi, describe, it, expect, beforeEach } from "vitest";
import type { PromptEntry } from "@/lib/prompts";

// ── Polyfill: ResizeObserver (not available in jsdom) ──────────────────────

globalThis.ResizeObserver = class ResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
} as unknown as typeof globalThis.ResizeObserver;

// ── Mock: next/navigation ──────────────────────────────────────────────────

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));

// ── Mock: lucide-react (heavy icon library — stub all named exports) ────────

vi.mock("lucide-react", () => {
  const Icon = () => null;
  const Loader2 = ({ className }: { className?: string }) => <svg className={className} />;
  return {
    Activity: Icon, AlertCircle: Icon, AlertTriangle: Icon, Archive: Icon,
    ArrowDownRight: Icon, ArrowLeft: Icon, ArrowRight: Icon, ArrowUpRight: Icon,
    BarChart3: Icon, Bell: Icon, BookOpen: Icon, Bot: Icon, Box: Icon, Brain: Icon,
    Calendar: Icon, Camera: Icon, Check: Icon, CheckCircle: Icon, CheckCircle2: Icon,
    CheckIcon: Icon, ChevronDown: Icon, ChevronDownIcon: Icon, ChevronLeft: Icon,
    ChevronRight: Icon, ChevronRightIcon: Icon, ChevronUp: Icon, Circle: Icon,
    CircleCheckIcon: Icon, CircleIcon: Icon, Clock: Icon, Copy: Icon,
    CornerDownRight: Icon, Cpu: Icon, Database: Icon, DollarSign: Icon,
    Download: Icon, Edit3: Icon, ExternalLink: Icon, Eye: Icon,
    FileCode: Icon, FileCode2: Icon, FilePlus: Icon, FileText: Icon,
    Folder: Icon, FolderMinus: Icon, Gamepad2: Icon, Gauge: Icon,
    GitBranch: Icon, Globe: Icon, Hammer: Icon, Hash: Icon, HeartPulse: Icon,
    History: Icon, Image: Icon, ImageIcon: Icon, InfoIcon: Icon,
    Key: Icon, KeyRound: Icon, Keyboard: Icon, Languages: Icon,
    Link: Icon, Link2: Icon, ListTodo: Icon, Loader: Icon, Loader2,
    Loader2Icon: Icon, LogOut: Icon, LucideProps: Icon,
    Mail: Icon, Maximize2: Icon, Menu: Icon, MessageSquare: Icon,
    MessageSquareShare: Icon, Mic: Icon, Minimize2: Icon, Minus: Icon,
    Monitor: Icon, Moon: Icon, MoreHorizontal: Icon, Network: Icon,
    OctagonXIcon: Icon, PanelLeftIcon: Icon, PanelRight: Icon,
    Paperclip: Icon, Pencil: Icon, Phone: Icon, Pin: Icon, PinOff: Icon,
    Play: Icon, Plus: Icon, Power: Icon, PowerOff: Icon, Radio: Icon,
    RefreshCw: Icon, RotateCcw: Icon, Save: Icon, Search: Icon, Send: Icon,
    Settings: Icon, Settings2: Icon, Shield: Icon, ShieldAlert: Icon,
    ShieldCheck: Icon, Square: Icon, Stethoscope: Icon, Sun: Icon,
    Tag: Icon, ThumbsDown: Icon, ThumbsUp: Icon, Timer: Icon, Trash2: Icon,
    TrendingDown: Icon, TrendingUp: Icon, TriangleAlertIcon: Icon,
    Unlink: Icon, User: Icon, UserCheck: Icon, UserX: Icon,
    Volume2: Icon, VolumeX: Icon, Webhook: Icon, Wifi: Icon, WifiOff: Icon,
    Wrench: Icon, XCircle: Icon, XIcon: Icon, Zap: Icon,
    ZoomIn: Icon, ZoomOut: Icon,
  };
});

// ── Mock: sonner toast ─────────────────────────────────────────────────────

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

// ── Mock: use-tool-progress ────────────────────────────────────────────────

vi.mock("@/hooks/use-tool-progress", () => ({
  useToolProgress: () => 0,
}));

// ── Mock: stores ───────────────────────────────────────────────────────────
// NOTE: @/hooks/use-translation is intentionally left UNMOCKED — the real
// i18n tables back both the "chat.prompts_section" label and the static
// welcome-screen fallback text these tests assert on.

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = {
        token: "test-token",
        isAuthenticated: true,
        version: "1.0.0",
        agents: ["Agent1", "Bob"],
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

// Store action mocks are hoisted to module scope (not recreated per getState()
// call) so a test can grab useChatStore.getState().sendMessage etc. and assert
// on the SAME mock instance the component invoked internally.
const storeActionMocks = {
  regenerate: vi.fn(),
  clearError: vi.fn(),
  sendMessage: vi.fn(),
  deleteMessage: vi.fn().mockResolvedValue(undefined),
  editMessage: vi.fn(),
  exportSession: vi.fn(),
  stopStream: vi.fn(),
  newChat: vi.fn(),
  setThinkingLevel: vi.fn(),
  setVoiceTurnPending: vi.fn(),
};

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
        currentAgent: "Agent1",
        agents: { Agent1: agentState },
      };
      return selector ? selector(state) : state;
    },
    {
      getState: () => ({
        currentAgent: "Agent1",
        agents: { Agent1: { activeSessionId: null, activeSessionIds: [], messageSource: { mode: "new-chat" }, connectionPhase: "idle" } },
        ...storeActionMocks,
      }),
    },
  ),
  isActivePhase: () => false,
  convertHistory: () => [],
  MAX_INPUT_LENGTH: 32000,
}));

// ── Mock: @/hooks/use-commands ──────────────────────────────────────────────
// Includes a "compact" no-arg command so the "prompt named like a command
// must not shadow it" scenario has a real command to collide with.

vi.mock("@/hooks/use-commands", () => ({
  useCommands: () => ({
    data: [
      { name: "new", description: "Start a new chat", category: "session", aliases: [], args: [] },
      { name: "compact", description: "Compact the session", category: "session", aliases: [], args: [] },
    ],
  }),
}));

// ── Mock: @/lib/prompts ──────────────────────────────────────────────────
// Keeps the real `parsePrompts` (tested directly, unmocked) but replaces
// `usePrompts` with a test-controlled value so each `it` can set the
// workspace prompt library contents without a network round-trip.

let mockPrompts: PromptEntry[] = [];

vi.mock("@/lib/prompts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/prompts")>();
  return {
    ...actual,
    usePrompts: () => ({ prompts: mockPrompts, isLoading: false }),
  };
});

// ── Mock: @/lib/queries ────────────────────────────────────────────────────

vi.mock("@/lib/queries", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/queries")>();
  return {
    ...actual,
    useSessions: () => ({ sessions: [], total: 0, isLoading: false, isFetched: true, fetchNextPage: vi.fn(), hasNextPage: false, isFetchingNextPage: false }),
    useSessionMessages: () => ({ data: { messages: [] }, isLoading: false, error: null, refetch: vi.fn() }),
    useAgents: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
    useProviders: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
    useProviderModels: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
    useProviderActive: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
  };
});

// ── Mock: @/lib/sanitize-url ───────────────────────────────────────────────

vi.mock("@/lib/sanitize-url", () => ({
  sanitizeUrl: (url: string) => url,
}));

// ── Mock: @/lib/api ────────────────────────────────────────────────────────
// usePrompts is mocked wholesale above, so apiGet is never actually called by
// the component tree in these tests — stubbed only so unrelated imports of
// "@/lib/api" don't explode.

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

// ── Mock: markdown and rich-card ───────────────────────────────────────────

vi.mock("@/components/ui/markdown", () => ({
  Markdown: ({ children }: { children: string }) => <div data-testid="markdown">{children}</div>,
}));

vi.mock("@/components/ui/rich-card", () => ({
  TableCard: ({ data }: { data: unknown }) => <div data-testid="table-card">{JSON.stringify(data)}</div>,
  MetricCard: ({ data }: { data: unknown }) => <div data-testid="metric-card">{JSON.stringify(data)}</div>,
}));

// ── Import under test ───────────────────────────────────────────────────────

import { parsePrompts } from "@/lib/prompts";
import { useChatStore } from "@/stores/chat-store";
import { ChatWelcomeScreen } from "@/app/(authenticated)/chat/ChatWelcomeScreen";
import { loadDraft } from "@/app/(authenticated)/chat/composer/draft";

beforeEach(() => {
  mockPrompts = [];
  vi.clearAllMocks();
});

// ── parsePrompts (pure parser) ──────────────────────────────────────────────

describe("parsePrompts", () => {
  it("parses two '## Heading' sections into title/body entries", () => {
    const md = [
      "## Weekly report",
      "Draft this week's report for the team.",
      "",
      "## Compact",
      "Summarize the conversation in 3 bullet points.",
    ].join("\n");
    expect(parsePrompts(md)).toEqual([
      { title: "Weekly report", body: "Draft this week's report for the team." },
      { title: "Compact", body: "Summarize the conversation in 3 bullet points." },
    ]);
  });

  it("returns [] for a file with no '##' headings", () => {
    expect(parsePrompts("Just some plain notes, no headings here.")).toEqual([]);
  });

  it("returns [] for an empty file", () => {
    expect(parsePrompts("")).toEqual([]);
  });

  it("drops a heading with no body", () => {
    const md = ["## Empty heading", "", "## Has body", "Some content."].join("\n");
    expect(parsePrompts(md)).toEqual([{ title: "Has body", body: "Some content." }]);
  });
});

// ── Composer: picking a prompt replaces input, never sends ─────────────────

describe("Composer prompt pick (Task 14)", () => {
  it("replaces the composer text with the prompt body and does NOT send", async () => {
    mockPrompts = [{ title: "draft_email", body: "Write a professional email about {topic}." }];
    const { ChatThread } = await import("@/app/(authenticated)/chat/ChatThread");
    const store = useChatStore.getState() as unknown as { sendMessage: ReturnType<typeof vi.fn> };
    render(
      <ChatThread streamError={null} isReadOnly={false} onClearError={vi.fn()} onRetry={vi.fn()} />,
    );
    const composerContainer = document.querySelector("[data-composer-input]");
    const textarea = composerContainer?.querySelector("textarea") as HTMLTextAreaElement;
    fireEvent.input(textarea, { target: { value: "/draft" } });

    // Scoped to the slash-menu listbox — the empty-state welcome screen
    // behind it also renders this prompt's title as a suggestion chip.
    const listbox = within(screen.getByRole("listbox"));

    // Rendered WITHOUT the leading "/" (it's a prompt row, not a command).
    expect(listbox.getByText("draft_email")).toBeInTheDocument();
    expect(listbox.queryByText("/draft_email")).not.toBeInTheDocument();

    fireEvent.mouseDown(listbox.getByText("draft_email"));

    expect(textarea.value).toBe("Write a professional email about {topic}.");
    expect(store.sendMessage).not.toHaveBeenCalled();
  });

  it("a prompt named like a real command ('compact') does not shadow it: both rows show, prompt pick still doesn't send", async () => {
    mockPrompts = [{ title: "compact", body: "Summarize the conversation in 3 bullet points." }];
    const { ChatThread } = await import("@/app/(authenticated)/chat/ChatThread");
    const store = useChatStore.getState() as unknown as { sendMessage: ReturnType<typeof vi.fn> };
    render(
      <ChatThread streamError={null} isReadOnly={false} onClearError={vi.fn()} onRetry={vi.fn()} />,
    );
    const composerContainer = document.querySelector("[data-composer-input]");
    const textarea = composerContainer?.querySelector("textarea") as HTMLTextAreaElement;
    fireEvent.input(textarea, { target: { value: "/compact" } });

    const listbox = within(screen.getByRole("listbox"));

    // Both rows are visible — the prompt does not hide/replace the command.
    expect(listbox.getByText("/compact")).toBeInTheDocument();
    expect(listbox.getByText("compact")).toBeInTheDocument();

    fireEvent.mouseDown(listbox.getByText("compact"));
    expect(textarea.value).toBe("Summarize the conversation in 3 bullet points.");
    expect(store.sendMessage).not.toHaveBeenCalled();
  });

  it("picking the '/compact' command row still dispatches the command (not shadowed by the prompt)", async () => {
    mockPrompts = [{ title: "compact", body: "Summarize the conversation in 3 bullet points." }];
    const { ChatThread } = await import("@/app/(authenticated)/chat/ChatThread");
    const store = useChatStore.getState() as unknown as { sendMessage: ReturnType<typeof vi.fn> };
    render(
      <ChatThread streamError={null} isReadOnly={false} onClearError={vi.fn()} onRetry={vi.fn()} />,
    );
    const composerContainer = document.querySelector("[data-composer-input]");
    const textarea = composerContainer?.querySelector("textarea") as HTMLTextAreaElement;
    fireEvent.input(textarea, { target: { value: "/compact" } });

    const listbox = within(screen.getByRole("listbox"));
    fireEvent.mouseDown(listbox.getByText("/compact"));
    // "compact" has no args and isn't a client-side shortcut (/stop, /new,
    // /think) — dispatchSlashCommand falls through to store.sendMessage.
    expect(store.sendMessage).toHaveBeenCalledWith("/compact");
    expect(textarea.value).toBe("");
  });
});

// ── Welcome screen: prompt-sourced suggestions with static fallback ────────

describe("ChatWelcomeScreen prompt suggestions (Task 14)", () => {
  it("renders the first 3 prompts as suggestion chips when the library is non-empty", () => {
    mockPrompts = [
      { title: "p1", body: "b1" },
      { title: "p2", body: "b2" },
      { title: "p3", body: "b3" },
      { title: "p4", body: "b4" },
    ];
    render(<ChatWelcomeScreen />);
    expect(screen.getByText("p1")).toBeInTheDocument();
    expect(screen.getByText("p2")).toBeInTheDocument();
    expect(screen.getByText("p3")).toBeInTheDocument();
    expect(screen.queryByText("p4")).not.toBeInTheDocument();
  });

  it("falls back to the static suggestions when the prompt library is empty", () => {
    mockPrompts = [];
    render(<ChatWelcomeScreen />);
    expect(screen.getByText("What's new in the world?")).toBeInTheDocument();
    expect(screen.getByText("Search for information")).toBeInTheDocument();
    expect(screen.getByText("Create a new tool")).toBeInTheDocument();
  });

  it("clicking a prompt-sourced suggestion fills the composer draft instead of sending", () => {
    mockPrompts = [{ title: "p1", body: "Body for p1" }];
    const store = useChatStore.getState() as unknown as { sendMessage: ReturnType<typeof vi.fn> };
    render(<ChatWelcomeScreen />);
    fireEvent.click(screen.getByText("p1"));
    // Never auto-sends — the prompt lands in the composer as an editable draft.
    expect(store.sendMessage).not.toHaveBeenCalled();
    // currentAgent is mocked to "Agent1" in this suite's useChatStore stub.
    expect(loadDraft("Agent1")).toBe("Body for p1");
  });
});
