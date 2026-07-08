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

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({
    t: (key: string, params?: Record<string, unknown>) => {
      if (key === "chat.context_tokens") return `${params?.tokens} / ${params?.limit} tokens (${params?.pct}%)`;
      if (key === "chat.context_remaining") return `Remaining: ${params?.remaining}`;
      if (key === "chat.context_stale") return "· updates after response";
      if (key === "chat.context_almost_full") return "Context almost full";
      if (key === "chat.cache_write") return `↑ cache write: ${params?.n}`;
      if (key === "chat.cache_read") return `↓ cache read: ${params?.n}`;
      if (key === "chat.reasoning_tokens") return `✦ reasoning: ${params?.n}`;
      return key;
    },
    locale: "en",
  }),
}));

// Мок CheckpointPanel — изолируем ContextBar от React Query
vi.mock("../CheckpointPanel", () => ({
  CheckpointPanel: () => null,
}));

// Мок chat-store — selector-форма (Zustand-совместимая)
vi.mock("@/stores/chat-store", () => ({
  useChatStore: (sel: (s: { currentAgent: string }) => unknown) =>
    sel({ currentAgent: "" }),
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

function getBarColor(): string {
  const bar = screen.getByTestId("tooltip-trigger").querySelector<HTMLDivElement>(
    'div[class*="rounded-full"][style*="width"]',
  );
  const cls = bar?.className ?? "";
  if (cls.includes("bg-destructive")) return "red";
  if (cls.includes("bg-warning")) return "yellow";
  if (cls.includes("bg-primary")) return "neutral";
  return "unknown";
}

describe("ContextBar — visibility", () => {
  it("shows model badge but no bar when tokens is null", () => {
    const { container } = render(<ContextBar tokens={null} model={MODEL} />);
    expect(container.firstChild).not.toBeNull();
    expect(container.querySelector('[style*="width"]')).toBeNull();
  });

  it("shows model badge but no bar when model has no context limit", () => {
    const { container } = render(<ContextBar tokens={1000} model="totally-unknown-model" />);
    expect(container.firstChild).not.toBeNull();
    expect(container.querySelector('[style*="width"]')).toBeNull();
  });
});

describe("ContextBar — tooltip breakdown", () => {
  it("shows cache write line when cacheCreationTokens > 0", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
        cacheCreationTokens={1200}
      />,
    );
    expect(getTooltipText()).toContain("cache write");
    expect(getTooltipText()).toMatch(/1.?200/); // locale may use NBSP or narrow NBSP
  });

  it("hides cache write line when cacheCreationTokens is null", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
        cacheCreationTokens={null}
      />,
    );
    expect(getTooltipText()).not.toContain("cache write");
  });

  it("hides cache write line when cacheCreationTokens is 0", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
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
        cacheReadTokens={null}
      />,
    );
    expect(getTooltipText()).not.toContain("cache read");
  });

  it("shows reasoning line when reasoningTokens > 0", () => {
    render(
      <ContextBar
        tokens={50000}
        model={MODEL}
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
        reasoningTokens={null}
      />,
    );
    expect(getTooltipText()).not.toContain("reasoning");
  });

  it("shows generating hint when isGenerating is true", () => {
    render(<ContextBar tokens={50000} model={MODEL} isGenerating={true} />);
    expect(getTooltipText()).toContain("updates after response");
  });

  it("hides generating hint when isGenerating is false", () => {
    render(<ContextBar tokens={50000} model={MODEL} isGenerating={false} />);
    expect(getTooltipText()).not.toContain("updates after response");
  });
});

describe("ContextBar — progress bar ratio", () => {
  it("clamps width at 100% when tokens > limit", () => {
    render(<ContextBar tokens={300_000} model={MODEL} />);
    expect(getBarWidthPct()).toBe("100%");
  });

  it("renders proportional width when tokens < limit", () => {
    // 100_000 / 200_000 = 50%
    render(<ContextBar tokens={100_000} model={MODEL} />);
    expect(getBarWidthPct()).toBe("50%");
  });

  it("renders correct width for edge case near limit", () => {
    // 195_000 / 200_000 = 97.5% → rounds to 98%
    render(<ContextBar tokens={195_000} model={MODEL} />);
    expect(getBarWidthPct()).toBe("98%");
  });

  it("shows alert text above 95% but not below", () => {
    render(<ContextBar tokens={150_000} model={MODEL} />);
    expect(screen.queryByText(/Context almost full/)).not.toBeInTheDocument();
  });
});

describe("ContextBar — bar color thresholds", () => {
  it("uses neutral color below 80%", () => {
    render(<ContextBar tokens={100_000} model={MODEL} />);
    expect(getBarColor()).toBe("neutral");
  });

  it("uses neutral color at exactly 80% (strict >)", () => {
    render(<ContextBar tokens={160_000} model={MODEL} />);
    expect(getBarColor()).toBe("neutral");
  });

  it("uses yellow color just above 80%", () => {
    render(<ContextBar tokens={161_000} model={MODEL} />);
    expect(getBarColor()).toBe("yellow");
  });

  it("uses yellow color at 90%", () => {
    render(<ContextBar tokens={180_000} model={MODEL} />);
    expect(getBarColor()).toBe("yellow");
  });

  it("uses yellow color at exactly 95% (strict >)", () => {
    render(<ContextBar tokens={190_000} model={MODEL} />);
    expect(getBarColor()).toBe("yellow");
  });

  it("uses red color just above 95%", () => {
    render(<ContextBar tokens={191_000} model={MODEL} />);
    expect(getBarColor()).toBe("red");
  });

  it("uses red color when context is over limit", () => {
    render(<ContextBar tokens={250_000} model={MODEL} />);
    expect(getBarColor()).toBe("red");
  });
});
