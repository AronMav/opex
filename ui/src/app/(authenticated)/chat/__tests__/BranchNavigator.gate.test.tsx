/**
 * Fix L (UI): BranchNavigator disables BOTH arrows when `disabled` is true
 * (a turn is active for the session). When idle, arrows follow the normal
 * boundary rules (first/last disabled).
 */
import { describe, it, expect, vi } from "vitest";
import { render } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/stores/chat-store", () => ({
  useChatStore: (selector?: (s: Record<string, unknown>) => unknown) => {
    const state = { switchBranch: vi.fn() };
    return selector ? selector(state) : state;
  },
}));

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

vi.mock("lucide-react", () => ({
  ChevronLeft: () => null,
  ChevronRight: () => null,
}));

import { BranchNavigator } from "@/app/(authenticated)/chat/BranchNavigator";

const siblings = [{ id: "s0" }, { id: "s1" }, { id: "s2" }];

describe("BranchNavigator — Fix L gate", () => {
  it("disables both arrows when disabled=true (mid-turn), even on a middle sibling", () => {
    const { getAllByRole } = render(
      <BranchNavigator parentMessageId="p" siblings={siblings} currentIndex={1} disabled />,
    );
    const buttons = getAllByRole("button");
    expect(buttons).toHaveLength(2);
    expect(buttons[0]).toBeDisabled();
    expect(buttons[1]).toBeDisabled();
  });

  it("enables navigation on a middle sibling when idle (disabled=false)", () => {
    const { getAllByRole } = render(
      <BranchNavigator parentMessageId="p" siblings={siblings} currentIndex={1} />,
    );
    const buttons = getAllByRole("button");
    expect(buttons[0]).not.toBeDisabled();
    expect(buttons[1]).not.toBeDisabled();
  });
});
