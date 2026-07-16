import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { MessageEditForm } from "../MessageEditForm";

describe("MessageEditForm (B4.1)", () => {
  it("renders full-width textarea with initial text", () => {
    render(<MessageEditForm initialText="hello" onSubmit={() => {}} onCancel={() => {}} />);
    const ta = screen.getByRole("textbox") as HTMLTextAreaElement;
    expect(ta.value).toBe("hello");
  });

  it("Enter submits, Shift+Enter does not, Escape cancels", () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();
    render(<MessageEditForm initialText="hi" onSubmit={onSubmit} onCancel={onCancel} />);
    const ta = screen.getByRole("textbox");
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: true });
    expect(onSubmit).not.toHaveBeenCalled();
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledWith("hi");
    fireEvent.keyDown(ta, { key: "Escape" });
    expect(onCancel).toHaveBeenCalled();
  });
});
