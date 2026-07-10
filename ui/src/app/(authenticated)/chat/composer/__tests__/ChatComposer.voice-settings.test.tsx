import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";

// H5: the voice-settings popover is a custom overlay — it must close on Escape
// and keep focus manageable (focus moves in on open, returns to trigger on close).

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

// stt provider present → voice controls (incl. the settings gear) render.
vi.mock("@/lib/queries", () => ({
  useProviderActive: () => ({ data: [{ capability: "stt", provider_name: "whisper" }] }),
  useAgents: () => ({ data: [] }),
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

describe("ChatComposer voice-settings popover (H5)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("opens on click and closes on Escape", () => {
    render(<ChatComposer />);
    const trigger = screen.getByRole("button", { name: "chat.voice_settings" });
    fireEvent.click(trigger);

    const panel = screen.getByRole("dialog");
    expect(panel).toBeInTheDocument();

    fireEvent.keyDown(panel, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(document.activeElement).toBe(trigger);
  });
});
