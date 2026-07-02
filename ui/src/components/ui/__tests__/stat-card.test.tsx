import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { StatCard } from "../stat-card";

test("renders label, value and sub", () => {
  render(<StatCard label="Tokens" value="1.2k" sub="today" />);
  expect(screen.getByText("Tokens")).toBeInTheDocument();
  expect(screen.getByText("1.2k")).toBeInTheDocument();
  expect(screen.getByText("today")).toBeInTheDocument();
});

test("accent applies the chart-token color to the value", () => {
  render(<StatCard label="Cost" value="$5" accent={3} />);
  expect(screen.getByText("$5")).toHaveClass("text-chart-3");
});
