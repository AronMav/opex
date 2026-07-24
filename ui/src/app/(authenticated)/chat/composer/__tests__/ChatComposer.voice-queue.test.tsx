import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

// Task 4: voice send during streaming must QUEUE instead of interrupting the
// running turn. Previously handleAutoResult/handleMicClick always inserted the
// transcript and called formRef.requestSubmit() immediately — which, via
// sendMessage's interrupt-aware branch, aborted the in-flight turn and lost its
// work. Now: while streaming, the transcript goes into the pending-message
// FIFO queue (voice: true) instead of submitting; a second voice result during
// the same turn is queued as a SEPARATE entry (FIFO — same store logic as
// stream-control.ts), and the drain combines them on idle.

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

// current agent has both stt+tts capabilities → voice controls render.
vi.mock("@/lib/queries", () => ({
  useAgents: () => ({
    data: [{ name: "main", capabilities: { text: true, stt: true, tts: true, vision: false, imagegen: false, websearch: false } }],
  }),
  useProviders: () => ({ data: [] }),
  useProviderModels: () => ({ data: [] }),
  useProviderModelsDetailed: () => ({ data: [] }),
}));

vi.mock("@/hooks/use-commands", () => ({
  useCommands: () => ({ data: [] }),
}));

vi.mock("@/lib/prompts", () => ({
  usePrompts: () => ({ prompts: [], isLoading: false }),
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

// Minimal fake store. queueMessage mirrors the REAL implementation in
// stream-control.ts (FIFO push — a second voice message becomes a separate
// queue entry) so the "second reply queues separately" assertion exercises the
// same logic the store uses, not just "was queueMessage called".
type PendingMessageEntry = { content: string; attachments?: unknown; voice?: boolean };
type PendingMessage = PendingMessageEntry[];

const chatState = {
  currentAgent: "main",
  agents: {
    main: {
      messageSource: { mode: "new-chat" },
      connectionPhase: "streaming" as string,
      pendingMessage: [] as PendingMessage,
      voiceTurnPending: false,
    },
  },
  sendMessage: vi.fn(),
  clearPending: vi.fn(() => {
    chatState.agents.main.pendingMessage = [];
  }),
  setVoiceTurnPending: vi.fn((pending: boolean) => {
    chatState.agents.main.voiceTurnPending = pending;
  }),
  queueMessage: vi.fn((text: string, attachments?: unknown, opts?: { voice?: boolean }) => {
    chatState.agents.main.pendingMessage.push({
      content: text,
      attachments,
      voice: opts?.voice === true,
    });
  }),
};

vi.mock("@/stores/chat-store", () => ({
  useChatStore: Object.assign(
    (selector?: (s: typeof chatState) => unknown) => (selector ? selector(chatState) : chatState),
    { getState: () => chatState },
  ),
  isActivePhase: (p?: string) => p === "streaming" || p === "submitted" || p === "reconnecting",
}));

// Capture the onAutoResult callback the composer wires into the voice-recorder
// hook, so the test can simulate a VAD auto-stop result directly.
let capturedOnAutoResult: ((text: string) => void) | null = null;
let capturedStop = vi.fn();
vi.mock("../../hooks/use-voice-recorder", () => ({
  useVoiceRecorder: (opts: { onAutoResult: (text: string) => void }) => {
    capturedOnAutoResult = opts.onAutoResult;
    return { state: "idle", start: vi.fn(), stop: capturedStop, elapsed: 0, level: 0 };
  },
}));

import { ChatComposer } from "../ChatComposer";

describe("ChatComposer voice queue during streaming (Task 4)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    chatState.agents.main.connectionPhase = "streaming";
    chatState.agents.main.pendingMessage = [];
    chatState.agents.main.voiceTurnPending = false;
    capturedOnAutoResult = null;
    capturedStop = vi.fn();
  });

  it("queues a VAD voice result during streaming instead of submitting", () => {
    render(<ChatComposer />);
    expect(capturedOnAutoResult).toBeTruthy();

    capturedOnAutoResult?.("привет");

    expect(chatState.queueMessage).toHaveBeenCalledWith("привет", undefined, { voice: true });
    expect(chatState.sendMessage).not.toHaveBeenCalled();
    expect(chatState.agents.main.pendingMessage).toEqual([
      { content: "привет", attachments: undefined, voice: true },
    ]);
  });

  it("queues a second voice result as a separate FIFO entry (no \\n append)", () => {
    render(<ChatComposer />);

    capturedOnAutoResult?.("первая фраза");
    capturedOnAutoResult?.("вторая фраза");

    expect(chatState.queueMessage).toHaveBeenCalledTimes(2);
    expect(chatState.agents.main.pendingMessage).toEqual([
      { content: "первая фраза", attachments: undefined, voice: true },
      { content: "вторая фраза", attachments: undefined, voice: true },
    ]);
    expect(chatState.sendMessage).not.toHaveBeenCalled();
  });

  it("shows the voice_queued indicator while a voice message is pending", () => {
    chatState.agents.main.pendingMessage = [{ content: "hi", voice: true }];
    render(<ChatComposer />);
    expect(screen.getByRole("status")).toHaveTextContent("chat.voice_queued");
  });

  it("still submits immediately when NOT streaming", () => {
    chatState.agents.main.connectionPhase = "idle";
    render(<ChatComposer />);

    capturedOnAutoResult?.("привет");

    expect(chatState.queueMessage).not.toHaveBeenCalled();
    expect(chatState.setVoiceTurnPending).toHaveBeenCalledWith(true, "main");
    expect(chatState.sendMessage).toHaveBeenCalledWith("привет", []);
  });
});
