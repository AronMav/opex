import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, waitFor } from "@testing-library/react";

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

// ChatComposer renders <ModelDropdown agent={currentAgent} /> unconditionally.
// ModelDropdown calls useAgents/useProviders/useProviderModels from @/lib/queries —
// none of which the factory mock below exports — so the real component would throw
// `useAgents is not a function` and crash the render. Stub it to null.
vi.mock("../ModelDropdown", () => ({
  ModelDropdown: () => null,
}));

// Mock @/lib/queries providing ALL hooks ChatComposer's subtree calls so no hook
// throws "is not a function".
vi.mock("@/lib/queries", () => ({
  useProviderActive: () => ({ data: [] }),
  useAgents: () => ({ data: [] }),
  useProviders: () => ({ data: [] }),
  useProviderModels: () => ({ data: [] }),
}));

// Capture the props FileActionButtons is rendered with.
const fabSpy = vi.fn();
vi.mock("../FileActionButtons", () => ({
  FileActionButtons: (props: Record<string, unknown>) => {
    fabSpy(props);
    return <div data-testid="fab" data-upload={String(props.uploadId)} />;
  },
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
      messageSource: { mode: "history", sessionId: "sess-9" },
      connectionPhase: "idle",
      pendingMessage: null,
    },
  },
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

const UPLOAD_UUID = "abc-123-uuid";

describe("ChatComposer captures upload row UUID", () => {
  const realFetch = global.fetch;
  beforeEach(() => {
    vi.clearAllMocks();
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      // url is a served path, filename is the row UUID (R1).
      json: async () => ({ url: "/uploads/something-else.ogg", filename: UPLOAD_UUID, size: 10 }),
    }) as unknown as typeof fetch;
  });
  afterEach(() => {
    global.fetch = realFetch;
  });

  it("passes uploadId = response.filename (the row UUID), not the URL path", async () => {
    const { container } = render(<ChatComposer />);
    const input = container.querySelector('input[type="file"]') as HTMLInputElement;
    const file = new File(["x"], "voice.ogg", { type: "audio/ogg" });
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input.dispatchEvent(new Event("change", { bubbles: true }));

    await waitFor(() => expect(screen.getByTestId("fab")).toBeInTheDocument());
    const props = fabSpy.mock.calls.at(-1)![0];
    expect(props.uploadId).toBe(UPLOAD_UUID);
    expect(String(props.uploadId)).not.toContain("/uploads/");
  });
});
