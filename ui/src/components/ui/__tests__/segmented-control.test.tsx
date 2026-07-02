import { test, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { SegmentedControl } from "../segmented-control";

const OPTS = [
  { value: "open", label: "Open" },
  { value: "restricted", label: "Restricted" },
] as const;

test("marks the active option checked", () => {
  render(<SegmentedControl value="open" onChange={() => {}} options={OPTS as any} />);
  expect(screen.getByRole("radio", { name: "Open" })).toHaveAttribute("aria-checked", "true");
  expect(screen.getByRole("radio", { name: "Restricted" })).toHaveAttribute("aria-checked", "false");
});

test("calls onChange with the clicked value", () => {
  const onChange = vi.fn();
  render(<SegmentedControl value="open" onChange={onChange} options={OPTS as any} />);
  fireEvent.click(screen.getByRole("radio", { name: "Restricted" }));
  expect(onChange).toHaveBeenCalledWith("restricted");
});
