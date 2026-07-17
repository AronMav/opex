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
  expect(onPick).toHaveBeenCalledWith({ kind: "command", name: "status", description: "Show status" });
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
  expect(onPick).toHaveBeenCalledWith({ kind: "command", name: "summarize_video", description: "Summarize a video" });
});

it("Tab picks the active option, like Enter", () => {
  const onPick = vi.fn();
  render(<CommandAutocomplete input="/s" commands={cmds} onPick={onPick} onClose={() => {}} />);

  fireEvent.keyDown(window, { key: "ArrowDown" });
  fireEvent.keyDown(window, { key: "Tab" });
  expect(onPick).toHaveBeenCalledWith({ kind: "command", name: "summarize_video", description: "Summarize a video" });
});

it("Shift+Tab does NOT pick — reverse focus navigation stays native", () => {
  const onPick = vi.fn();
  render(<CommandAutocomplete input="/s" commands={cmds} onPick={onPick} onClose={() => {}} />);

  fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
  expect(onPick).not.toHaveBeenCalled();
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

// ── Prompt library section (Task 14) ──────────────────────────────────────

const promptEntries = [
  { title: "compact", body: "Please compact the summary into 3 bullet points." },
  { title: "weekly_report", body: "Draft this week's report." },
];

it("renders a Prompts section below matching commands", () => {
  render(
    <CommandAutocomplete
      input="/"
      commands={cmds}
      prompts={promptEntries}
      onPick={() => {}}
      onClose={() => {}}
    />,
  );
  expect(screen.getByText("/status")).toBeInTheDocument();
  expect(screen.getByText("compact")).toBeInTheDocument();
  expect(screen.getByText("weekly_report")).toBeInTheDocument();
});

it("prompt rows render without the leading slash", () => {
  render(
    <CommandAutocomplete
      input="/comp"
      commands={cmds}
      prompts={promptEntries}
      onPick={() => {}}
      onClose={() => {}}
    />,
  );
  expect(screen.getByText("compact")).toBeInTheDocument();
  expect(screen.queryByText("/compact")).not.toBeInTheDocument();
});

it("a prompt named like a real command does NOT shadow it — both rows are shown and each picks its own kind", () => {
  const onPick = vi.fn();
  const commandsWithCompact = [
    ...cmds,
    { name: "compact", description: "Compact the session", category: "session", aliases: [], args: [] },
  ];
  render(
    <CommandAutocomplete
      input="/compact"
      commands={commandsWithCompact}
      prompts={promptEntries}
      onPick={onPick}
      onClose={() => {}}
    />,
  );

  // Both the "/compact" command row and the "compact" prompt row are visible.
  const commandRow = screen.getByText("/compact");
  const promptRow = screen.getByText("compact");
  expect(commandRow).toBeInTheDocument();
  expect(promptRow).toBeInTheDocument();

  // Picking the command row picks the command, not the prompt.
  fireEvent.mouseDown(commandRow);
  expect(onPick).toHaveBeenCalledWith({ kind: "command", name: "compact", description: "Compact the session" });
  onPick.mockClear();

  // Picking the prompt row picks the prompt.
  fireEvent.mouseDown(promptRow);
  expect(onPick).toHaveBeenCalledWith({
    kind: "prompt",
    title: "compact",
    body: "Please compact the summary into 3 bullet points.",
  });
});
