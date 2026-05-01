// ── ContextBar.test.tsx ─────────────────────────────────────────────────────
// Phase 2 todo #8 — UI rendering coverage for the context window indicator.
// Verifies tooltip breakdown (cache write / cache read / reasoning lines) is
// emitted only when the corresponding field is > 0, and that the bar width
// is clamped at 100% when tokens exceed the model limit.

import React from "react";
import { describe, it, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

// Bypass Radix Portal/animation gating: render Tooltip pieces inline so tests
// can assert the breakdown text directly without simulating hover state.
vi.mock("@/components/ui/tooltip", () => ({
  TooltipProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  Tooltip: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ children, asChild: _asChild, ...rest }: any) => (
    <div data-testid="tooltip-trigger" {...rest}>
      {children}
    </div>
  ),
  TooltipContent: ({ children, ...rest }: any) => (
    <div data-testid="tooltip-content" {...rest}>
      {children}
    </div>
  ),
}));

import { ContextBar } from "../ContextBar";

const MODEL = "claude-opus-4"; // 200k limit per model-limits.ts

function getTooltipText(): string {
  return screen.getByTestId("tooltip-content").textContent ?? "";
}

function getBarWidthPct(): string | undefined {
  const bar = screen.getByTestId("tooltip-trigger").querySelector<HTMLDivElement>(
    'div[class*="rounded-full"][style*="width"]',
  );
  return bar?.style.width;
}

describe("ContextBar — visibility", () => {
  it("returns null when tokens is null", () => {
    const { container } = render(<ContextBar tokens={null} model={MODEL} />);
    expect(container.firstChild).toBeNull();
  });

  it("returns null when model is unknown (no context limit)", () => {
    const { container } = render(<ContextBar tokens={1000} model="totally-unknown-model" />);
    expect(container.firstChild).toBeNull();
  });
});

describe("ContextBar — tooltip breakdown", () => {
  it("shows cache write line when cacheCreationTokens > 0", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
        outputTokens={1000}
        cacheCreationTokens={1200}
      />,
    );
    expect(getTooltipText()).toContain("cache write");
    expect(getTooltipText()).toContain("1 200"); // ru-RU formatting (NBSP)
  });

  it("hides cache write line when cacheCreationTokens is null", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
        outputTokens={1000}
        cacheCreationTokens={null}
      />,
    );
    expect(getTooltipText()).not.toContain("cache write");
  });

  it("hides cache write line when cacheCreationTokens is 0 (provider reports 'no cache')", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
        outputTokens={1000}
        cacheCreationTokens={0}
      />,
    );
    expect(getTooltipText()).not.toContain("cache write");
  });

  it("shows cache read line when cacheReadTokens > 0", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
        outputTokens={1000}
        cacheReadTokens={8200}
      />,
    );
    expect(getTooltipText()).toContain("cache read");
  });

  it("hides cache read line when cacheReadTokens is null", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
        outputTokens={1000}
        cacheReadTokens={null}
      />,
    );
    expect(getTooltipText()).not.toContain("cache read");
  });

  it("shows reasoning line only when both outputTokens > 0 and reasoningTokens > 0", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
        outputTokens={1800}
        reasoningTokens={600}
      />,
    );
    expect(getTooltipText()).toContain("reasoning");
  });

  it("hides reasoning line when reasoningTokens is null", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
        outputTokens={1800}
        reasoningTokens={null}
      />,
    );
    expect(getTooltipText()).not.toContain("reasoning");
  });

  it("hides Output line entirely when outputTokens is null", () => {
    render(<ContextBar tokens={50000} model={MODEL} outputTokens={null} />);
    // Russian "Output:" line is gated on `outputTokens != null && > 0`.
    expect(getTooltipText()).not.toContain("Output:");
  });
});

describe("ContextBar — progress bar ratio", () => {
  it("clamps width at 100% when tokens > limit", () => {
    // 300_000 / 200_000 = 1.5 → must be clamped to 100%
    render(<ContextBar tokens={300_000} model={MODEL} />);
    expect(getBarWidthPct()).toBe("100%");
  });

  it("renders proportional width when tokens < limit", () => {
    // 100_000 / 200_000 = 50%
    render(<ContextBar tokens={100_000} model={MODEL} />);
    expect(getBarWidthPct()).toBe("50%");
  });

  it("shows the 'almost full' warning label when ratio > 95%", () => {
    render(<ContextBar tokens={195_000} model={MODEL} />);
    // Russian copy in component: "Контекст почти заполнен"
    expect(screen.getByText(/Контекст почти заполнен/)).toBeInTheDocument();
  });

  it("hides warning label below the 95% threshold", () => {
    render(<ContextBar tokens={150_000} model={MODEL} />);
    expect(screen.queryByText(/Контекст почти заполнен/)).not.toBeInTheDocument();
  });
});
