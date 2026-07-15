import { vi, describe, it, expect } from "vitest";
import "@testing-library/jest-dom/vitest";

// ── Polyfills ──────────────────────────────────────────────────────────────
globalThis.ResizeObserver = class ResizeObserver {
  observe() {} unobserve() {} disconnect() {}
} as unknown as typeof globalThis.ResizeObserver;
globalThis.IntersectionObserver = class IntersectionObserver {
  constructor() {} observe() {} unobserve() {} disconnect() {}
} as unknown as typeof globalThis.IntersectionObserver;
Element.prototype.scrollIntoView = vi.fn();

// ── Mocks ──────────────────────────────────────────────────────────────────
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));
vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));
vi.mock("@/lib/queries", () => ({
  useSessions: () => ({ data: { sessions: [] }, isLoading: false, error: null, refetch: vi.fn() }),
  useSessionMessages: () => ({ data: { messages: [] }, isLoading: false, error: null, refetch: vi.fn() }),
  useProviderActive: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
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
  queryClient: { invalidateQueries: vi.fn(), setQueryData: vi.fn(), getQueryData: () => undefined },
}));
vi.mock("@tanstack/react-query", async () => {
  const actual = await vi.importActual("@tanstack/react-query");
  return {
    ...actual,
    useQueryClient: () => ({ invalidateQueries: vi.fn(), setQueryData: vi.fn() }),
    useQuery: () => ({ data: undefined, isLoading: false, error: null, refetch: vi.fn() }),
  };
});

// ── Imports under test (after mocks) ──────────────────────────────────────
import React from "react";
import { render, screen } from "@testing-library/react";
import { MessageItem } from "@/app/(authenticated)/chat/MessageItem";
import { useChatStore } from "@/stores/chat-store";
import type { ChatMessage } from "@/stores/chat-store";

// ── Helpers ────────────────────────────────────────────────────────────────

function seedStore() {
  useChatStore.setState((draft) => {
    draft.currentAgent = "TestAgent";
    draft.agents["TestAgent"] = {
      activeSessionId: null,
      messageSource: { mode: "new-chat" },
      streamError: null,
      connectionPhase: "idle",
      connectionError: null,
      forceNewSession: false,
      boundaryMessageId: null,
      activeSessionIds: [],
      renderLimit: 100,
      modelOverride: null,
      turnLimitMessage: null,
      streamGeneration: 0,
      reconnectAttempt: 0,
      maxReconnectAttempts: 3,
      isLlmReconnecting: false,
      lastEventId: null,
      selectedBranches: {},
      pendingMessage: null,
      contextTokens: null,
      contextOutputTokens: null,
      cacheReadTokens: null,
      cacheCreationTokens: null,
      reasoningTokens: null,
      hasMoreHistory: false,
      isLoadingHistory: false,
      modelContextLimit: null,
    };
  });
}

function makeAssistantMessage(overrides?: Partial<ChatMessage>): ChatMessage {
  return {
    id: "test-msg-1",
    role: "assistant",
    parts: [{ type: "text", text: "hello" }],
    agentId: "TestAgent",
    createdAt: new Date(Date.now() - 60_000).toISOString(),
    ...overrides,
  };
}

// ── Tests ──────────────────────────────────────────────────────────────────

describe("mirror badge", () => {
  it("renders '↩ cron' badge when isMirror is true", () => {
    seedStore();
    render(<MessageItem message={makeAssistantMessage({ isMirror: true })} />);
    expect(screen.getByText("↩ cron")).toBeTruthy();
  });

  it("does not render badge when isMirror is false", () => {
    seedStore();
    render(<MessageItem message={makeAssistantMessage({ isMirror: false })} />);
    expect(screen.queryByText("↩ cron")).toBeNull();
  });

  it("does not render badge when isMirror is absent", () => {
    seedStore();
    render(<MessageItem message={makeAssistantMessage()} />);
    expect(screen.queryByText("↩ cron")).toBeNull();
  });
});
