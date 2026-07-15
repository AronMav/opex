import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

// Fixes a live prod regression: after the Profiles project narrowed
// provider_active to embedding-only, ChatComposer's mic gate (hasSttProvider,
// derived from provider_active[stt]) was permanently false → the mic button
// was hidden for every agent. Voice controls must be gated on the CURRENT
// AGENT's `capabilities` (AgentInfo.capabilities) instead.

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "ru" }),
}));

vi.mock("@/lib/api", () => ({
  assertToken: () => "test-token",
  apiGet: vi.fn(),
  apiPost: vi.fn(),
}));

vi.mock("../ModelDropdown", () => ({ ModelDropdown: () => null }));

// Mutable agent list so each test can flip capabilities before render.
let mockAgentList: Array<{ name: string; capabilities: { stt: boolean; tts: boolean } }> = [];

vi.mock("@/lib/queries", () => ({
  useAgents: () => ({ data: mockAgentList }),
  useProviders: () => ({ data: [] }),
  useProviderModels: () => ({ data: [] }),
  useProviderModelsDetailed: () => ({ data: [] }),
}));

vi.mock("@/hooks/use-commands", () => ({
  useCommands: () => ({ data: [] }),
}));

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = { agents: ["main"], token: "test-token" };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token", currentAgent: "main" }) },
  ),
}));

const chatState = {
  currentAgent: "main",
  agents: {
    main: {
      messageSource: { mode: "new-chat" },
      connectionPhase: "idle",
      pendingMessage: null,
    },
  },
  sendMessage: vi.fn(),
  queueMessage: vi.fn(),
};
const useChatStore: any = (selector?: (s: typeof chatState) => unknown) =>
  selector ? selector(chatState) : chatState;
useChatStore.getState = () => chatState;
vi.mock("@/stores/chat-store", () => ({
  useChatStore: (selector?: (s: typeof chatState) => unknown) => useChatStore(selector),
  isActivePhase: (p?: string) => p === "streaming" || p === "connecting",
}));

vi.mock("../../hooks/use-voice-recorder", () => ({
  useVoiceRecorder: () => ({ state: "idle", start: vi.fn(), stop: vi.fn(), elapsed: 0, level: 0 }),
}));

import { ChatComposer } from "../ChatComposer";

describe("ChatComposer voice gating on agent capabilities", () => {
  beforeEach(() => vi.clearAllMocks());

  it("stt=true, tts=false: mic shows, hands-free toggle is absent", () => {
    mockAgentList = [
      { name: "main", capabilities: { stt: true, tts: false } },
    ];
    render(<ChatComposer />);
    expect(screen.getByRole("button", { name: "chat.voice_input" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "chat.continuous_voice" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "chat.voice_settings" })).not.toBeInTheDocument();
  });

  it("stt=false: mic is absent", () => {
    mockAgentList = [
      { name: "main", capabilities: { stt: false, tts: false } },
    ];
    render(<ChatComposer />);
    expect(screen.queryByRole("button", { name: "chat.voice_input" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "chat.continuous_voice" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "chat.voice_settings" })).not.toBeInTheDocument();
  });
});
