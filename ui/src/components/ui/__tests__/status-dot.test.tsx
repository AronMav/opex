import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render } from "@testing-library/react";
import { StatusDot } from "../status-dot";

test("success maps to bg-success", () => {
  const { container } = render(<StatusDot status="success" />);
  expect(container.firstChild).toHaveClass("bg-success");
});

test("error maps to bg-destructive", () => {
  const { container } = render(<StatusDot status="error" />);
  expect(container.firstChild).toHaveClass("bg-destructive");
});

test("pulse adds animate-pulse", () => {
  const { container } = render(<StatusDot status="success" pulse />);
  expect(container.firstChild).toHaveClass("animate-pulse");
});
