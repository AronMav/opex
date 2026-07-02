import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { StatusBadge } from "../status-badge";

test("online maps to success tone", () => {
  render(<StatusBadge status="online">Online</StatusBadge>);
  const el = screen.getByText("Online");
  expect(el).toHaveAttribute("data-variant", "success");
});

test("error maps to destructive", () => {
  render(<StatusBadge status="error">Err</StatusBadge>);
  expect(screen.getByText("Err")).toHaveAttribute("data-variant", "destructive");
});

test("unknown status falls back to secondary", () => {
  render(<StatusBadge status="whatever">?</StatusBadge>);
  expect(screen.getByText("?")).toHaveAttribute("data-variant", "secondary");
});

test("falls back to the status string when no children", () => {
  render(<StatusBadge status="paused" />);
  expect(screen.getByText("paused")).toBeInTheDocument();
});
