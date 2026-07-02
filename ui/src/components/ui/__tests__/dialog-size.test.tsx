import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { Dialog, DialogContent } from "../dialog";

test("size xl sets the xl max-width", () => {
  render(
    <Dialog open>
      <DialogContent size="xl">body</DialogContent>
    </Dialog>,
  );
  expect(screen.getByText("body")).toHaveClass("sm:max-w-xl");
});

test("default size is lg", () => {
  render(
    <Dialog open>
      <DialogContent>body</DialogContent>
    </Dialog>,
  );
  expect(screen.getByText("body")).toHaveClass("sm:max-w-lg");
});
