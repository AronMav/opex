import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { TimeoutsSection } from "../TimeoutsSection";
import { useLanguageStore } from "@/stores/language-store";

beforeEach(() => {
  useLanguageStore.setState({ locale: "en" });
});

describe("TimeoutsSection", () => {
  it("renders four inputs with defaults", () => {
    render(<TimeoutsSection value={{}} onChange={() => {}} />);
    expect(screen.getByLabelText(/connect/i)).toHaveValue(10);
    expect(screen.getByLabelText(/request \(non-streaming\)/i)).toHaveValue(120);
    expect(screen.getByLabelText(/stream inactivity/i)).toHaveValue(60);
    expect(screen.getByLabelText(/stream max duration/i)).toHaveValue(600);
  });

  it("emits partial timeouts on edit", () => {
    const onChange = vi.fn();
    render(<TimeoutsSection value={{}} onChange={onChange} />);
    fireEvent.change(screen.getByLabelText(/request \(non-streaming\)/i), {
      target: { value: "45" },
    });
    expect(onChange).toHaveBeenCalled();
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1][0];
    expect(lastCall.request_secs).toBe(45);
  });

  it("rejects connect_secs = 0 client-side", () => {
    render(<TimeoutsSection value={{ connect_secs: 0 }} onChange={() => {}} />);
    expect(screen.getByText(/must be >=\s*1/i)).toBeInTheDocument();
  });
});
