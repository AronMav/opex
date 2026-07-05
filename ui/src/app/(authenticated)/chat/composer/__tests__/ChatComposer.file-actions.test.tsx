import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, waitFor } from "@testing-library/react";

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

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));

// Mock @/lib/queries providing ALL hooks ChatComposer's subtree calls.
vi.mock("@/lib/queries", () => ({
  useProviderActive: () => ({ data: [] }),
  useAgents: () => ({ data: [] }),
  useProviders: () => ({ data: [] }),
  useProviderModels: () => ({ data: [] }),
  useProviderModelsDetailed: () => ({ data: [] }),
}));

// ChatComposer renders <ModelDropdown agent={currentAgent} /> unconditionally
// (ChatComposer.tsx line 859). ModelDropdown.tsx calls useAgents/useProviders/
// useProviderModels from @/lib/queries — none of which the factory mock above
// exports — so the real component would throw `useAgents is not a function` and
// crash the render before any `fab` element mounts. Stub it to null.
vi.mock("../ModelDropdown", () => ({
  ModelDropdown: () => null,
}));

// Capture the props FileActionButtons is rendered with.
const fabSpy = vi.fn();
vi.mock("../FileActionButtons", () => ({
  FileActionButtons: (props: Record<string, unknown>) => {
    fabSpy(props);
    return <div data-testid="fab" data-upload={String(props.uploadId)} data-mime={String(props.mime)} />;
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

// Chat store: provide currentAgent + a per-agent slot with an active session id.
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

describe("ChatComposer file action buttons", () => {
  const realFetch = global.fetch;
  beforeEach(() => {
    vi.clearAllMocks();
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ url: "/uploads/served-path.ogg", filename: UPLOAD_UUID, size: 10 }),
    }) as unknown as typeof fetch;
  });
  afterEach(() => {
    global.fetch = realFetch;
  });

  it("renders FileActionButtons per attachment with uploadId(UUID) + mime + agent + session", async () => {
    const { container } = render(<ChatComposer />);
    // Simulate an uploaded attachment via the hidden file input.
    const input = container.querySelector('input[type="file"]') as HTMLInputElement;
    const file = new File(["x"], "voice.ogg", { type: "audio/ogg" });
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input.dispatchEvent(new Event("change", { bubbles: true }));

    await waitFor(() => expect(screen.getByTestId("fab")).toBeInTheDocument());
    const props = fabSpy.mock.calls.at(-1)![0];
    expect(props.uploadId).toBe(UPLOAD_UUID);
    expect(String(props.uploadId)).not.toContain("/uploads/");
    expect(props.mime).toBe("audio/ogg");
    expect(props.agent).toBe("main");
    expect(props.sessionId).toBe("sess-9");
  });
});
