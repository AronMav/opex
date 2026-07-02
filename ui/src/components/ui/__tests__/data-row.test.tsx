import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { DataRow } from "../data-row";

test("renders title, subtitle, children and actions", () => {
  render(
    <DataRow
      leading={<span data-testid="lead" />}
      title="my-hook"
      subtitle="created today"
      actions={<button>del</button>}
    >
      <span data-testid="center" />
    </DataRow>,
  );
  expect(screen.getByText("my-hook")).toBeInTheDocument();
  expect(screen.getByText("created today")).toBeInTheDocument();
  expect(screen.getByTestId("lead")).toBeInTheDocument();
  expect(screen.getByTestId("center")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "del" })).toBeInTheDocument();
});

test("muted dims the row", () => {
  const { container } = render(<DataRow muted title="x" />);
  expect(container.querySelector("[data-slot='card']")).toHaveClass("opacity-60");
});
