import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { IconTile } from "../icon-tile";

test("renders its icon child", () => {
  render(<IconTile><svg data-testid="ic" /></IconTile>);
  expect(screen.getByTestId("ic")).toBeInTheDocument();
});

test("default tone is primary", () => {
  render(<IconTile data-testid="tile"><svg /></IconTile>);
  expect(screen.getByTestId("tile")).toHaveClass("bg-primary/10");
});

test("tone=success applies success surface", () => {
  render(<IconTile tone="success" data-testid="tile"><svg /></IconTile>);
  expect(screen.getByTestId("tile")).toHaveClass("bg-success/10");
});
