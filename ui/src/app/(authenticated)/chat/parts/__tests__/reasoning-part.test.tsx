import React from "react";
import { describe, it, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

// C1: reasoning is collapsed by default for FINISHED reasoning, auto-expanded
// while streaming; the pulse dot animates ONLY while streaming.

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));
vi.mock("@/components/ui/message", () => ({
  MessageContent: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="reasoning-body">{children}</div>
  ),
}));

import { ReasoningPart } from "../ReasoningPart";

describe("ReasoningPart (C1)", () => {
  it("collapses finished reasoning (body not rendered) with a static dot", () => {
    const { container } = render(<ReasoningPart text="final thoughts" streaming={false} />);
    // Radix Collapsible unmounts content when closed.
    expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
    // No pulsing dot when finished.
    expect(container.querySelector(".animate-pulse")).toBeNull();
    // Trigger + label still present.
    expect(screen.getByText("chat.reasoning")).toBeInTheDocument();
  });

  it("auto-expands while streaming (body rendered) with a pulsing dot", () => {
    const { container } = render(<ReasoningPart text="thinking…" streaming />);
    expect(screen.getByTestId("reasoning-body")).toHaveTextContent("thinking…");
    expect(container.querySelector(".animate-pulse")).not.toBeNull();
  });

  it("collapses when streaming transitions to finished", () => {
    const { container, rerender } = render(<ReasoningPart text="t" streaming />);
    expect(screen.getByTestId("reasoning-body")).toBeInTheDocument();
    rerender(<ReasoningPart text="t" streaming={false} />);
    expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
    expect(container.querySelector(".animate-pulse")).toBeNull();
  });

  it("defaults to collapsed (streaming prop omitted)", () => {
    render(<ReasoningPart text="x" />);
    expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
  });
});
