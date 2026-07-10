import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { it, expect, vi } from "vitest";
import { CommandAutocomplete } from "./command-autocomplete";

const cmds = [
  { name: "status", description: "Show status", category: "status", aliases: [], args: [] },
  { name: "summarize_video", description: "Summarize a video", category: "media", aliases: [], args: [{ name: "url" }] },
];

it("filters commands by prefix after slash", () => {
  render(<CommandAutocomplete input="/sum" commands={cmds} onPick={() => {}} onClose={() => {}} />);
  expect(screen.getByText(/summarize_video/)).toBeInTheDocument();
  expect(screen.queryByText(/status/)).not.toBeInTheDocument();
});

it("renders nothing without leading slash", () => {
  const { container } = render(<CommandAutocomplete input="hello" commands={cmds} onPick={() => {}} onClose={() => {}} />);
  expect(container).toBeEmptyDOMElement();
});

it("renders nothing when no commands match", () => {
  const { container } = render(<CommandAutocomplete input="/zzz" commands={cmds} onPick={() => {}} onClose={() => {}} />);
  expect(container).toBeEmptyDOMElement();
});

it("calls onPick on mousedown click", () => {
  const onPick = vi.fn();
  render(<CommandAutocomplete input="/sta" commands={cmds} onPick={onPick} onClose={() => {}} />);
  fireEvent.mouseDown(screen.getByText("/status"));
  expect(onPick).toHaveBeenCalledWith("status");
});

// ── Keyboard navigation ──────────────────────────────────────────────────

it("ArrowDown moves the active option, Enter picks it", () => {
  const onPick = vi.fn();
  // "/s" matches both "status" and "summarize_video" (alphabetical filter order
  // preserves the input array order: status, summarize_video).
  render(<CommandAutocomplete input="/s" commands={cmds} onPick={onPick} onClose={() => {}} />);

  const options = screen.getAllByRole("option");
  expect(options).toHaveLength(2);
  expect(options[0]).toHaveAttribute("aria-selected", "true");
  expect(options[1]).toHaveAttribute("aria-selected", "false");

  fireEvent.keyDown(window, { key: "ArrowDown" });
  expect(options[0]).toHaveAttribute("aria-selected", "false");
  expect(options[1]).toHaveAttribute("aria-selected", "true");

  fireEvent.keyDown(window, { key: "Enter" });
  expect(onPick).toHaveBeenCalledWith("summarize_video");
});

it("ArrowUp wraps to the last option", () => {
  render(<CommandAutocomplete input="/s" commands={cmds} onPick={() => {}} onClose={() => {}} />);
  const options = screen.getAllByRole("option");
  fireEvent.keyDown(window, { key: "ArrowUp" });
  expect(options[0]).toHaveAttribute("aria-selected", "false");
  expect(options[1]).toHaveAttribute("aria-selected", "true");
});

it("Escape calls onClose", () => {
  const onClose = vi.fn();
  render(<CommandAutocomplete input="/sta" commands={cmds} onPick={() => {}} onClose={onClose} />);
  fireEvent.keyDown(window, { key: "Escape" });
  expect(onClose).toHaveBeenCalled();
});

it("exposes listbox/option a11y roles", () => {
  render(<CommandAutocomplete input="/sta" commands={cmds} onPick={() => {}} onClose={() => {}} />);
  expect(screen.getByRole("listbox")).toBeInTheDocument();
  expect(screen.getByRole("option")).toBeInTheDocument();
});
