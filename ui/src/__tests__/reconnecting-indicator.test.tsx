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
    render(<ReconnectingIndicator />);
    expect(screen.getByRole("status")).toBeInTheDocument();
    expect(screen.getByText(/Reconnecting/)).toBeInTheDocument();
  });

  it("has a static aria-label (T8 removed the attempt counter)", () => {
    render(<ReconnectingIndicator />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-label", "Reconnecting");
  });

  it("has aria-live polite for screen readers", () => {
    render(<ReconnectingIndicator />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-live", "polite");
  });
});
