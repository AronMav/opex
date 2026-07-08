import { describe, it, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

// Drive selectLiveAssistantText via a controlled fake state.
const fakeState = {
  agents: {
    A: {
      connectionPhase: "streaming",
      messageSource: {
        mode: "live",
        messages: [{ id: "a1", role: "assistant", parts: [{ type: "text", text: "" }] }],
      },
    },
  },
};
vi.mock("@/stores/chat-store", () => ({
  useChatStore: (selector: (s: unknown) => unknown) => selector(fakeState),
}));
vi.mock("zustand/react/shallow", () => ({ useShallow: (fn: unknown) => fn }));

import { StreamingAnnouncer } from "../StreamingAnnouncer";

describe("StreamingAnnouncer", () => {
  it("renders a single sr-only polite status region", () => {
    render(<StreamingAnnouncer agent="A" />);
    const region = screen.getByRole("status");
    expect(region).toHaveAttribute("aria-live", "polite");
    expect(region).toHaveAttribute("aria-atomic", "true");
    expect(region).toHaveAccessibleName("chat.response_announcer");
    expect(region.className).toContain("sr-only");
  });
});
