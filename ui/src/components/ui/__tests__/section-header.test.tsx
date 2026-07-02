import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { SectionHeader } from "../section-header";

test("renders title, description and actions", () => {
  render(<SectionHeader title="Accounts" description="OAuth" actions={<button>Add</button>} />);
  expect(screen.getByText("Accounts")).toBeInTheDocument();
  expect(screen.getByText("OAuth")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Add" })).toBeInTheDocument();
});

test("renders count when provided", () => {
  render(<SectionHeader title="Bindings" count={3} />);
  expect(screen.getByText("3")).toBeInTheDocument();
});
