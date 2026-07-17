import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, waitFor, fireEvent } from "@testing-library/react";

// C2 regression: with the @-mention menu open, ArrowDown+Enter must SELECT the
// active mention (insert "@name ") and NOT submit the half-typed "@" message.

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

// Two agents so MentionAutocomplete renders (filtered peers = ["Beta"]).
vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = { agents: ["main", "Beta"], token: "test-token" };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token", currentAgent: "main" }) },
  ),
}));

const sendMessage = vi.fn();
const queueMessage = vi.fn();
const chatState = {
  currentAgent: "main",
  agents: {
    main: {
      messageSource: { mode: "history", sessionId: "sess-9" },
      connectionPhase: "idle",
      pendingMessage: null,
    },
  },
  sendMessage,
  queueMessage,
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

import { render as rtlRender } from "@testing-library/react";
import { ChatComposer } from "../ChatComposer";

describe("ChatComposer a11y (C2)", () => {
  it("textarea exposes an accessible name via aria-label", () => {
    const { getByRole } = rtlRender(<ChatComposer />);
    expect(getByRole("textbox", { name: /message/i })).toBeInTheDocument();
  });
});

// Set an uncontrolled textarea's value the way the component does internally,
// then dispatch the input event so React's onInput handler runs.
function typeInto(ta: HTMLTextAreaElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
  setter.call(ta, value);
  ta.dispatchEvent(new Event("input", { bubbles: true }));
}

describe("ChatComposer @-mention keyboard nav (C2)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("Enter with the mention menu open inserts the mention and does NOT send", async () => {
    const { container } = render(<ChatComposer />);
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;

    // Trigger the mention menu: value ending in "@".
    typeInto(ta, "hey @");

    // The listbox appears with the peer agent "Beta".
    await waitFor(() => {
      expect(container.querySelector('[role="listbox"]')).toBeInTheDocument();
    });
    const options = container.querySelectorAll('[role="option"]');
    expect(options.length).toBe(1);
    expect(options[0].textContent).toContain("@Beta");

    // ArrowDown (wrap to same single option) then Enter — capture-phase handler
    // in MentionAutocomplete selects it. Fire on window since the listener is
    // registered with { capture: true } on window.
    fireEvent.keyDown(window, { key: "ArrowDown" });
    fireEvent.keyDown(window, { key: "Enter" });

    // Mention inserted → textarea now contains "@Beta ", menu closed.
    await waitFor(() => {
      expect(ta.value).toContain("@Beta ");
    });
    expect(container.querySelector('[role="listbox"]')).not.toBeInTheDocument();

    // Critically: the half-typed "@" message was NOT sent.
    expect(sendMessage).not.toHaveBeenCalled();
  });

  it("wires aria-controls from the textarea to the mention listbox (L5)", async () => {
    const { container } = render(<ChatComposer />);
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    typeInto(ta, "hey @");
    await waitFor(() => {
      expect(container.querySelector('[role="listbox"]')).toBeInTheDocument();
    });
    const listbox = container.querySelector('[role="listbox"]') as HTMLElement;
    const controls = ta.getAttribute("aria-controls");
    expect(controls).toBeTruthy();
    expect(listbox.id).toBe(controls);
  });

  it("textarea Enter keydown is suppressed while the mention menu is open (no requestSubmit send)", async () => {
    const { container } = render(<ChatComposer />);
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    typeInto(ta, "ping @");
    await waitFor(() => {
      expect(container.querySelector('[role="listbox"]')).toBeInTheDocument();
    });

    // React onKeyDown on the textarea: with the menu open it must preventDefault
    // and NOT requestSubmit → sendMessage stays untouched.
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: false });
    expect(sendMessage).not.toHaveBeenCalled();
  });
});
