import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { CompressionDivider } from "../CompressionDivider";

test("renders segment label with correct numbers", () => {
  render(<CompressionDivider segmentIndex={2} totalSegments={3} />);
  expect(screen.getByText(/Сегмент 2 из 3/)).toBeInTheDocument();
});

test("renders compression marker text", () => {
  render(<CompressionDivider segmentIndex={1} totalSegments={2} />);
  expect(screen.getByText(/Контекст сжат/)).toBeInTheDocument();
});
