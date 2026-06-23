import { describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { ReconnectingIndicator } from "@/components/chat/ReconnectingIndicator";
import { useLanguageStore } from "@/stores/language-store";

beforeEach(() => {
  useLanguageStore.setState({ locale: "en" });
});

describe("ReconnectingIndicator", () => {
  it("renders pulsating dot and reconnecting text", () => {
    render(<ReconnectingIndicator attempt={1} maxAttempts={3} />);
    expect(screen.getByRole("status")).toBeInTheDocument();
    expect(screen.getByText(/Reconnecting/)).toBeInTheDocument();
  });

  it("shows attempt count", () => {
    render(<ReconnectingIndicator attempt={2} maxAttempts={3} />);
    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.getByText(/\/3/)).toBeInTheDocument();
  });

  it("has correct aria-label with attempt info", () => {
    render(<ReconnectingIndicator attempt={2} maxAttempts={3} />);
    expect(screen.getByRole("status")).toHaveAttribute(
      "aria-label",
      "Reconnecting, attempt 2 of 3"
    );
  });

  it("has aria-live polite for screen readers", () => {
    render(<ReconnectingIndicator attempt={1} maxAttempts={3} />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-live", "polite");
  });
});
