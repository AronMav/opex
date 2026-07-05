import { test, expect } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { Dialog, DialogContent, DialogBody } from "../dialog";

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

test("extended size 2xl is available as a preset", () => {
  render(
    <Dialog open>
      <DialogContent size="2xl">body</DialogContent>
    </Dialog>,
  );
  expect(screen.getByText("body")).toHaveClass("sm:max-w-2xl");
});

test("default layout scrolls the whole content", () => {
  render(
    <Dialog open>
      <DialogContent>body</DialogContent>
    </Dialog>,
  );
  const el = screen.getByText("body");
  expect(el).toHaveClass("overflow-y-auto");
  expect(el).toHaveClass("p-6");
});

test("panel layout is a flex column with no padding and hidden overflow", () => {
  render(
    <Dialog open>
      <DialogContent layout="panel">body</DialogContent>
    </Dialog>,
  );
  const el = screen.getByText("body");
  expect(el).toHaveClass("flex");
  expect(el).toHaveClass("flex-col");
  expect(el).toHaveClass("overflow-hidden");
  expect(el).toHaveClass("p-0");
  expect(el).not.toHaveClass("p-6");
});

test("every dialog uses dvh for max-height (no legacy vh)", () => {
  render(
    <Dialog open>
      <DialogContent>body</DialogContent>
    </Dialog>,
  );
  expect(screen.getByText("body")).toHaveClass("max-h-[calc(100dvh-2rem)]");
});

test("DialogBody is an internal scroll region", () => {
  render(
    <Dialog open>
      <DialogContent layout="panel">
        <DialogBody>content</DialogBody>
      </DialogContent>
    </Dialog>,
  );
  const body = screen.getByText("content");
  expect(body).toHaveClass("flex-1");
  expect(body).toHaveClass("min-h-0");
  expect(body).toHaveClass("overflow-y-auto");
});
