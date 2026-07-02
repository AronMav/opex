import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { Card } from "../card";

test("renders children on a neu-card surface", () => {
  render(<Card>hello</Card>);
  const el = screen.getByText("hello");
  expect(el).toHaveClass("neu-card");
});

test("interactive adds hover elevation", () => {
  render(<Card interactive>x</Card>);
  expect(screen.getByText("x")).toHaveClass("neu-hover");
});

test("merges custom className", () => {
  render(<Card className="p-4">y</Card>);
  expect(screen.getByText("y")).toHaveClass("p-4");
});
