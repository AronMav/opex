import { describe, it, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ThreadErrorBoundary } from "../ThreadErrorBoundary";

// L6 + B1: a crashed thread must announce the failure (role=alert) and its Retry
// must re-drive the failed operation via onRetry — not merely re-mount the child.

function Boom(): never {
  throw new Error("kaboom");
}

describe("ThreadErrorBoundary (L6 + B1)", () => {
  it("exposes the error as an alert and calls onRetry on retry", () => {
    const onRetry = vi.fn();
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <ThreadErrorBoundary retryLabel="Retry" onRetry={onRetry}>
        <Boom />
      </ThreadErrorBoundary>,
    );

    expect(screen.getByRole("alert")).toHaveTextContent("kaboom");

    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetry).toHaveBeenCalledTimes(1);

    spy.mockRestore();
  });
});
