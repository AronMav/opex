import { render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { describe, it, expect, beforeEach, vi } from "vitest";

// M5 + M7: phase labels are i18n-driven (no baked-in bilingual map) and the
// indicator is a polite live region; the emoji is decorative (aria-hidden).
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import { useChatStore } from "@/stores/chat-store";
import { VideoProgressIndicator } from "@/components/chat/VideoProgressIndicator";

describe("VideoProgressIndicator", () => {
  beforeEach(() => useChatStore.setState({ videoProgress: {} } as never));

  it("renders a polite status region with a localized phase label", () => {
    const { rerender } = render(<VideoProgressIndicator sessionId="s1" />);
    expect(screen.queryByRole("status")).toBeNull();

    useChatStore.getState().setVideoProgress("s1", "saving", "💾 raw fallback");
    rerender(<VideoProgressIndicator sessionId="s1" />);

    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(status).toHaveTextContent("chat.video_phase_saving");

    // The emoji must live in an aria-hidden node so it is never announced.
    const hidden = Array.from(status.querySelectorAll('[aria-hidden="true"]'));
    expect(hidden.some((el) => el.textContent?.includes("💾"))).toBe(true);
  });
});
