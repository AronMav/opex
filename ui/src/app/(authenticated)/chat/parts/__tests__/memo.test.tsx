import { describe, it, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";

// P2: ReasoningPart and ClarifyCard are memoized so an unrelated parent re-render
// (very frequent during streaming) does not re-render these subtrees.
const counter = { renders: 0 };
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => {
    counter.renders++;
    return { t: (k: string) => k, locale: "en" };
  },
}));

import { ReasoningPart } from "../ReasoningPart";
import { ClarifyCard } from "@/components/chat/ClarifyCard";

describe("memoization (P2)", () => {
  it("ReasoningPart skips re-render when props are unchanged", () => {
    counter.renders = 0;
    const { rerender } = render(<div><ReasoningPart text="hello" streaming={false} /></div>);
    const first = counter.renders;
    expect(first).toBeGreaterThan(0);
    rerender(<div><ReasoningPart text="hello" streaming={false} /></div>);
    expect(counter.renders).toBe(first);
  });

  it("ClarifyCard is wrapped in React.memo", () => {
    expect((ClarifyCard as unknown as { $$typeof: symbol }).$$typeof).toBe(
      Symbol.for("react.memo"),
    );
  });
});
