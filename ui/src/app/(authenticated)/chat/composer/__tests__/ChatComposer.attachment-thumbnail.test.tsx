/**
 * Wave-2 Task 13b: composer image-attachment thumbnails.
 *
 * An image attachment chip renders an ImageLightbox thumbnail (self-trigger)
 * instead of the generic Paperclip icon; the remove-X stays a SIBLING of the
 * lightbox trigger (not nested inside it) so it keeps working independently.
 * A non-image attachment chip is unchanged (Paperclip + filename).
 */
import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, waitFor, fireEvent, screen } from "@testing-library/react";

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

describe("ChatComposer attachment chip thumbnails (13b)", () => {
  const realFetch = global.fetch;
  afterEach(() => {
    global.fetch = realFetch;
  });

  it("renders an <img> thumbnail for an image attachment, with remove-X as a sibling", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ url: "/uploads/photo.png", filename: "row-uuid-1", size: 10 }),
    }) as unknown as typeof fetch;

    const { container } = render(<ChatComposer />);
    const input = container.querySelector('input[type="file"]') as HTMLInputElement;
    const file = new File(["x"], "photo.png", { type: "image/png" });
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input.dispatchEvent(new Event("change", { bubbles: true }));

    const chip = await waitFor(() => {
      const el = container.querySelector('[data-upload-id="row-uuid-1"]');
      expect(el).toBeInTheDocument();
      return el as HTMLElement;
    });

    // Thumbnail: an <img> pointing at the uploaded file's served path.
    const img = chip.querySelector("img");
    expect(img).not.toBeNull();
    expect(img!.getAttribute("src")).toBe("/uploads/photo.png");

    // Remove button still present and functional, as a SIBLING of the
    // lightbox trigger (not nested inside its own trigger button).
    const removeBtn = screen.getByRole("button", { name: "chat.remove_attachment" });
    expect(removeBtn.contains(img)).toBe(false);
    fireEvent.click(removeBtn);
    await waitFor(() => {
      expect(container.querySelector('[data-upload-id="row-uuid-1"]')).not.toBeInTheDocument();
    });
  });

  it("keeps the Paperclip + filename layout for a non-image attachment", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ url: "/uploads/doc.pdf", filename: "row-uuid-2", size: 10 }),
    }) as unknown as typeof fetch;

    const { container } = render(<ChatComposer />);
    const input = container.querySelector('input[type="file"]') as HTMLInputElement;
    const file = new File(["x"], "doc.pdf", { type: "application/pdf" });
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input.dispatchEvent(new Event("change", { bubbles: true }));

    const chip = await waitFor(() => {
      const el = container.querySelector('[data-upload-id="row-uuid-2"]');
      expect(el).toBeInTheDocument();
      return el as HTMLElement;
    });

    expect(chip.querySelector("img")).toBeNull();
    expect(screen.getByText("doc.pdf")).toBeInTheDocument();

    const removeBtn = screen.getByRole("button", { name: "chat.remove_attachment" });
    fireEvent.click(removeBtn);
    await waitFor(() => {
      expect(container.querySelector('[data-upload-id="row-uuid-2"]')).not.toBeInTheDocument();
    });
  });
});
