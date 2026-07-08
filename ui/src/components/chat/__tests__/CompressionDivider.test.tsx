import { test, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { CompressionDivider } from "../CompressionDivider";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string, params?: Record<string, unknown>) => {
    if (key === "chat.compression_divider") return `Context compressed · Segment ${params?.current} of ${params?.total}`;
    if (key === "chat.segment") return `Segment ${params?.current} of ${params?.total}`;
    return key;
  }, locale: "en" }),
}));

test("renders segment label with correct numbers", () => {
  render(<CompressionDivider segmentIndex={2} totalSegments={3} />);
  expect(screen.getByText(/Segment 2 of 3/)).toBeInTheDocument();
});

test("renders compression marker text", () => {
  render(<CompressionDivider segmentIndex={1} totalSegments={2} />);
  expect(screen.getByText(/Context compressed/)).toBeInTheDocument();
});

test("exposes a separator role with a segment aria-label (M4)", () => {
  render(<CompressionDivider segmentIndex={2} totalSegments={3} />);
  const sep = screen.getByRole("separator");
  expect(sep).toHaveAttribute("aria-label", expect.stringMatching(/segment/i));
});
