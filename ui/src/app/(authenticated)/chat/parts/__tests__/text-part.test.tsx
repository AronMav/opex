import React from "react";
import { test, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

// NOTE: useChatStore is intentionally NOT mocked. The real Zustand store calls
// useSyncExternalStore internally (a real hook that runs BEFORE the early
// return), which is exactly the production condition that makes a conditional
// useSmoothedText crash on a hook-count mismatch. A no-op mock (0 hooks) would
// hide the bug because React uses the mount dispatcher after a 0-hook render.
vi.mock("@/lib/format", () => ({ cleanContent: (t: string) => t }));
vi.mock("@/components/ui/message", () => ({
  MessageContent: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="message-content">{children}</div>
  ),
}));

import { TextPart } from "../TextPart";

// Regression: useSmoothedText must be called on EVERY render (before any early
// return), or toggling highlightRanges off/on changes the hook count and React
// crashes the message with "Rendered more hooks than during the previous render".
// This reproduces the search → clear-search interaction on a single message.
test("survives highlightRanges toggling off without a rules-of-hooks crash", () => {
  const { rerender } = render(
    <TextPart text="hello world" highlightRanges={[{ start: 0, end: 5 }]} />,
  );
  // Highlight branch renders the raw text inside a <mark>.
  expect(screen.getByText("hello")).toBeInTheDocument();

  // Clearing the search drops highlightRanges on the SAME instance — the hook
  // count must stay stable so this does not throw.
  expect(() =>
    rerender(<TextPart text="hello world" />),
  ).not.toThrow();
  expect(screen.getByTestId("message-content")).toHaveTextContent("hello world");
});

// And the reverse transition (search activated on an already-mounted message).
test("survives highlightRanges toggling on without a rules-of-hooks crash", () => {
  const { rerender } = render(<TextPart text="hello world" />);
  expect(screen.getByTestId("message-content")).toHaveTextContent("hello world");

  expect(() =>
    rerender(
      <TextPart text="hello world" highlightRanges={[{ start: 0, end: 5 }]} />,
    ),
  ).not.toThrow();
  expect(screen.getByText("hello")).toBeInTheDocument();
});

// ── W4-3: loader roles — inline caret means "text is arriving here" ────────

test("renders inline streaming caret while streaming", () => {
  render(<TextPart text="Привет" streaming />);
  expect(screen.getByTestId("streaming-cursor")).toBeInTheDocument();
});

test("no caret when complete", () => {
  render(<TextPart text="Привет" />);
  expect(screen.queryByTestId("streaming-cursor")).toBeNull();
});
