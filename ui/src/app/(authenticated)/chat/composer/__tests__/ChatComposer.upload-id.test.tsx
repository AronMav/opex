import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, waitFor, fireEvent } from "@testing-library/react";

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
// Stub it to null so we don't need to provide all query context.
vi.mock("../ModelDropdown", () => ({
  ModelDropdown: () => null,
}));


// Mock @/lib/queries providing ALL hooks ChatComposer's subtree calls.
vi.mock("@/lib/queries", () => ({
  useProviderActive: () => ({ data: [] }),
  useAgents: () => ({ data: [] }),
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

// The upload API returns: url = served path, filename = the row UUID (R1 requirement).
const UPLOAD_UUID = "11111111-1111-1111-1111-111111111111";

describe("ChatComposer captures upload row UUID", () => {
  const realFetch = global.fetch;
  beforeEach(() => {
    vi.clearAllMocks();
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        url: "/uploads/some-served-path.ogg",
        filename: UPLOAD_UUID,
        size: 10,
      }),
    }) as unknown as typeof fetch;
  });
  afterEach(() => {
    global.fetch = realFetch;
  });

  it("sets data-upload-id on the attachment chip to response.filename (the row UUID, not the URL path)", async () => {
    const { container } = render(<ChatComposer />);
    const input = container.querySelector('input[type="file"]') as HTMLInputElement;
    const file = new File(["x"], "voice.ogg", { type: "audio/ogg" });
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input.dispatchEvent(new Event("change", { bubbles: true }));

    // Wait for the attachment chip to appear with the correct data-upload-id.
    await waitFor(() => {
      const chip = container.querySelector(`[data-upload-id="${UPLOAD_UUID}"]`);
      expect(chip).toBeInTheDocument();
    });

    // The upload-id must be the row UUID, not a /uploads/... path.
    const chip = container.querySelector(`[data-upload-id="${UPLOAD_UUID}"]`);
    expect(chip).not.toBeNull();
    expect(chip!.getAttribute("data-upload-id")).toBe(UPLOAD_UUID);
    expect(chip!.getAttribute("data-upload-id")).not.toContain("/uploads/");
  });

  it("adds every file from a multi-file drop (B2)", async () => {
    let n = 0;
    global.fetch = vi.fn().mockImplementation(async () => ({
      ok: true,
      json: async () => ({ url: `/uploads/p${n}.png`, filename: `uuid-${n++}`, size: 10 }),
    })) as unknown as typeof fetch;

    const { container } = render(<ChatComposer />);
    const dropZone = container.querySelector("[data-composer-input]") as HTMLElement;
    const f1 = new File(["a"], "a.png", { type: "image/png" });
    const f2 = new File(["b"], "b.png", { type: "image/png" });

    fireEvent.drop(dropZone, { dataTransfer: { files: [f1, f2] } });

    await waitFor(() => {
      expect(container.querySelectorAll("[data-upload-id]").length).toBe(2);
    });
  });
});
