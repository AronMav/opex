import { test, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { Pagination } from "../pagination";

test("shows current/total and disables prev on first page", () => {
  render(<Pagination page={1} total={5} onPrev={() => {}} onNext={() => {}} />);
  expect(screen.getByText("1 / 5")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /previous/i })).toBeDisabled();
  expect(screen.getByRole("button", { name: /next/i })).not.toBeDisabled();
});

test("disables next on last page", () => {
  render(<Pagination page={5} total={5} onPrev={() => {}} onNext={() => {}} />);
  expect(screen.getByRole("button", { name: /next/i })).toBeDisabled();
});

test("fires onNext when next clicked", () => {
  const onNext = vi.fn();
  render(<Pagination page={2} total={5} onPrev={() => {}} onNext={onNext} />);
  fireEvent.click(screen.getByRole("button", { name: /next/i }));
  expect(onNext).toHaveBeenCalledOnce();
});
