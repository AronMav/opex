import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { Badge } from "../badge";

test("size sm uses the 2xs type token", () => {
  render(<Badge size="sm">tag</Badge>);
  expect(screen.getByText("tag")).toHaveClass("text-2xs");
});

test("default size keeps text-xs", () => {
  render(<Badge>tag</Badge>);
  expect(screen.getByText("tag")).toHaveClass("text-xs");
});

test("size xs uses the 3xs type token", () => {
  render(<Badge size="xs">tag</Badge>);
  expect(screen.getByText("tag")).toHaveClass("text-3xs");
});

test("outline-primary variant keeps the primary tint", () => {
  render(<Badge variant="outline-primary">tag</Badge>);
  expect(screen.getByText("tag")).toHaveClass("text-primary");
});
