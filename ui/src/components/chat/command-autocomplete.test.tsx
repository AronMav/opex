import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { CommandAutocomplete } from "./command-autocomplete";

const cmds = [
  { name: "status", description: "Show status", category: "status", aliases: [], args: [] },
  { name: "summarize_video", description: "Summarize a video", category: "media", aliases: [], args: [{ name: "url" }] },
];

it("filters commands by prefix after slash", () => {
  render(<CommandAutocomplete input="/sum" commands={cmds} onPick={() => {}} />);
  expect(screen.getByText(/summarize_video/)).toBeInTheDocument();
  expect(screen.queryByText(/status/)).not.toBeInTheDocument();
});

it("renders nothing without leading slash", () => {
  const { container } = render(<CommandAutocomplete input="hello" commands={cmds} onPick={() => {}} />);
  expect(container).toBeEmptyDOMElement();
});
