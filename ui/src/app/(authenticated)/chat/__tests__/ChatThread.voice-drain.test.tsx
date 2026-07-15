/**
 * Exercises the REAL pendingMessage-drain effect in ChatThread.tsx (~:154-172):
 * when connectionPhase transitions to 'idle' (a clean turn end) and a voice
 * message is queued (pendingMessage.voice === true), the effect must call
 * `setVoiceTurnPending(true, agent)` BEFORE `sendMessage(...)` so
 * ChatComposer's spoken-reply effect (which reads the flag on the *next*
 * turn-end) is armed in time to speak the drained turn's reply.
 *
 * Mocking follows the existing full-ChatThread-render pattern used in
 * `ui/src/__tests__/chat-input.test.tsx` (mock next/navigation, lucide-react,
 * sonner, stores, lib/queries, etc., then render the real <ChatThread>). The
 * chat-store mock exposes a MUTABLE per-agent state object so the test can
 * flip `connectionPhase` between renders and force React to re-run the real
 * effect via `rerender` — this is the real drain effect, not a reimplementation.
 */

import { vi, describe, it, expect, beforeEach } from "vitest";
import { render } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// ── Mock: next/navigation ──────────────────────────────────────────────────

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));

// ── Mock: lucide-react (heavy icon library — stub all named exports) ────────

vi.mock("lucide-react", async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  const Icon = () => null;
  const stubbed: Record<string, unknown> = {};
  for (const key of Object.keys(actual)) {
    stubbed[key] = Icon;
  }
  return stubbed;
});

// ── Mock: sonner toast ─────────────────────────────────────────────────────

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

// ── Mock: translation hook ─────────────────────────────────────────────────

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

vi.mock("@/hooks/use-tool-progress", () => ({
  useToolProgress: () => 0,
}));

// ── Mock: auth store ────────────────────────────────────────────────────────

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = {
        token: "test-token",
        isAuthenticated: true,
        version: "1.0.0",
        agents: ["Agent1"],
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

// ── Mock: chat store — mutable per-agent state so the test can drive the
// real drain effect across renders (connectionPhase streaming → idle). ─────

const AGENT = "Agent1";

type PendingMessage = { content: string; attachments?: unknown; voice?: boolean } | null;

const agentState: {
  activeSessionId: string | null;
  activeSessionIds: string[];
  messageSource: { mode: string };
  streamError: string | null;
  connectionPhase: string;
  reconnectAttempt: number;
  maxReconnectAttempts: number;
  isLlmReconnecting: boolean;
  renderLimit: number;
  hasMoreHistory: boolean;
  isLoadingHistory: boolean;
  pendingMessage: PendingMessage;
  voiceTurnPending: boolean;
} = {
  activeSessionId: null,
  activeSessionIds: [],
  messageSource: { mode: "new-chat" },
  streamError: null,
  connectionPhase: "streaming",
  reconnectAttempt: 0,
  maxReconnectAttempts: 6,
  isLlmReconnecting: false,
  renderLimit: 100,
  hasMoreHistory: false,
  isLoadingHistory: false,
  pendingMessage: null,
  voiceTurnPending: false,
};

// storeActionMocks are hoisted to module scope (not recreated per getState()
// call) so the test can assert on the SAME mock instances the component
// invoked internally, and check call ORDER between them.
const storeActionMocks = {
  sendMessage: vi.fn(),
  setVoiceTurnPending: vi.fn((pending: boolean) => {
    agentState.voiceTurnPending = pending;
  }),
  clearPending: vi.fn(() => {
    agentState.pendingMessage = null;
  }),
  resumeStream: vi.fn(),
  loadEarlierMessages: vi.fn(),
  loadPreviousMessages: vi.fn(),
  newChat: vi.fn(),
  stopStream: vi.fn(),
  setThinkingLevel: vi.fn(),
};

vi.mock("@/stores/chat-store", () => ({
  useChatStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state: Record<string, unknown> = {
        currentAgent: AGENT,
        agents: { [AGENT]: agentState },
      };
      return selector ? selector(state) : state;
    },
    {
      getState: () => ({
        currentAgent: AGENT,
        agents: { [AGENT]: agentState },
        ...storeActionMocks,
      }),
    },
  ),
  isActivePhase: (p?: string) => p === "streaming" || p === "submitted" || p === "reconnecting",
  convertHistory: () => [],
  MAX_INPUT_LENGTH: 32000,
}));

// ── Mock: use-commands ──────────────────────────────────────────────────────

vi.mock("@/hooks/use-commands", () => ({
  useCommands: () => ({ data: [] }),
}));

// ── Mock: lib/queries ────────────────────────────────────────────────────────

vi.mock("@/lib/queries", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/queries")>();
  return {
    ...actual,
    useSessions: () => ({ data: { sessions: [] }, isLoading: false, error: null, refetch: vi.fn() }),
    useSessionMessages: () => ({ data: { messages: [] }, isLoading: false, error: null, refetch: vi.fn() }),
    useAgents: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
    useProviders: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
    useProviderModels: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
    useProviderActive: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
  };
});

vi.mock("@/lib/sanitize-url", () => ({
  sanitizeUrl: (url: string) => url,
}));

vi.mock("@/lib/api", () => ({
  apiGet: vi.fn().mockResolvedValue({}),
  apiPost: vi.fn().mockResolvedValue({}),
  apiPut: vi.fn().mockResolvedValue({}),
  apiDelete: vi.fn().mockResolvedValue(undefined),
  getToken: () => "test-token",
  assertToken: () => "test-token",
}));

vi.mock("@/lib/query-client", () => ({
  queryClient: { invalidateQueries: vi.fn(), setQueryData: vi.fn() },
}));

vi.mock("@tanstack/react-query", async () => {
  const actual = await vi.importActual("@tanstack/react-query");
  return {
    ...actual,
    useQueryClient: () => ({ invalidateQueries: vi.fn(), setQueryData: vi.fn() }),
    useQuery: () => ({ data: undefined, isLoading: false, error: null, refetch: vi.fn() }),
  };
});

vi.mock("zustand/react/shallow", () => ({
  useShallow: (fn: unknown) => fn,
}));

vi.mock("@/components/ui/markdown", () => ({
  Markdown: ({ children }: { children: string }) => <div data-testid="markdown">{children}</div>,
}));

vi.mock("@/components/ui/rich-card", () => ({
  TableCard: ({ data }: { data: unknown }) => <div data-testid="table-card">{JSON.stringify(data)}</div>,
  MetricCard: ({ data }: { data: unknown }) => <div data-testid="metric-card">{JSON.stringify(data)}</div>,
}));

// ── Import component under test ─────────────────────────────────────────────

import { ChatThread } from "@/app/(authenticated)/chat/ChatThread";

describe("ChatThread — pendingMessage drain effect (real effect, voice arming)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    agentState.connectionPhase = "streaming";
    agentState.pendingMessage = null;
    agentState.voiceTurnPending = false;
  });

  it("arms voiceTurnPending BEFORE sendMessage when draining a queued voice message on idle transition", () => {
    agentState.connectionPhase = "streaming";
    agentState.pendingMessage = { content: "привет", attachments: undefined, voice: true };

    const { rerender } = render(
      <ChatThread streamError={null} isReadOnly={false} onClearError={vi.fn()} onRetry={vi.fn()} />,
    );

    // No drain yet — still streaming.
    expect(storeActionMocks.sendMessage).not.toHaveBeenCalled();
    expect(storeActionMocks.setVoiceTurnPending).not.toHaveBeenCalled();

    // Clean transition to idle — drives the real effect.
    agentState.connectionPhase = "idle";
    rerender(<ChatThread streamError={null} isReadOnly={false} onClearError={vi.fn()} onRetry={vi.fn()} />);

    expect(storeActionMocks.setVoiceTurnPending).toHaveBeenCalledWith(true, AGENT);
    expect(storeActionMocks.sendMessage).toHaveBeenCalledWith("привет", undefined);
    expect(storeActionMocks.clearPending).toHaveBeenCalledWith(AGENT);

    // Order matters: voiceTurnPending must be armed before the drained turn starts.
    const setOrder = storeActionMocks.setVoiceTurnPending.mock.invocationCallOrder[0];
    const sendOrder = storeActionMocks.sendMessage.mock.invocationCallOrder[0];
    expect(setOrder).toBeLessThan(sendOrder);
  });

  it("does NOT arm voiceTurnPending when draining a non-voice queued message", () => {
    agentState.connectionPhase = "streaming";
    agentState.pendingMessage = { content: "напечатал текст", attachments: undefined, voice: false };

    const { rerender } = render(
      <ChatThread streamError={null} isReadOnly={false} onClearError={vi.fn()} onRetry={vi.fn()} />,
    );

    agentState.connectionPhase = "idle";
    rerender(<ChatThread streamError={null} isReadOnly={false} onClearError={vi.fn()} onRetry={vi.fn()} />);

    expect(storeActionMocks.setVoiceTurnPending).not.toHaveBeenCalled();
    expect(storeActionMocks.sendMessage).toHaveBeenCalledWith("напечатал текст", undefined);
    expect(storeActionMocks.clearPending).toHaveBeenCalledWith(AGENT);
  });

  it("discards the pending message on error transition without sending it", () => {
    agentState.connectionPhase = "streaming";
    agentState.pendingMessage = { content: "должно быть отброшено", attachments: undefined, voice: true };

    const { rerender } = render(
      <ChatThread streamError={null} isReadOnly={false} onClearError={vi.fn()} onRetry={vi.fn()} />,
    );

    agentState.connectionPhase = "error";
    rerender(<ChatThread streamError={null} isReadOnly={false} onClearError={vi.fn()} onRetry={vi.fn()} />);

    expect(storeActionMocks.sendMessage).not.toHaveBeenCalled();
    expect(storeActionMocks.setVoiceTurnPending).not.toHaveBeenCalled();
    expect(storeActionMocks.clearPending).toHaveBeenCalledWith(AGENT);
  });
});
