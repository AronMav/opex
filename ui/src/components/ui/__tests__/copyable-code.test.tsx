import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

const copyText = vi.fn().mockResolvedValue(undefined);
vi.mock("@/lib/clipboard", () => ({ copyText: (t: string) => copyText(t) }));

import { CopyableCode } from "../copyable-code";

beforeEach(() => copyText.mockClear());

test("shows the value", () => {
  render(<CopyableCode value="sk-123" />);
  expect(screen.getByText("sk-123")).toBeInTheDocument();
});

test("copies on click and flips to copied state", async () => {
  render(<CopyableCode value="sk-123" />);
  fireEvent.click(screen.getByRole("button", { name: /copy/i }));
  expect(copyText).toHaveBeenCalledWith("sk-123");
  await waitFor(() =>
    expect(screen.getByRole("button", { name: /copied/i })).toBeInTheDocument(),
  );
});
