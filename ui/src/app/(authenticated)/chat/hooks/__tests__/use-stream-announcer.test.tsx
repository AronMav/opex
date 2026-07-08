import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useStreamAnnouncer } from "../use-stream-announcer";

describe("useStreamAnnouncer", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("announces completed sentences on a throttle, withholding the tail", () => {
    const { result, rerender } = renderHook(
      ({ id, text, streaming }) => useStreamAnnouncer(id, text, streaming),
      { initialProps: { id: "m1", text: "", streaming: true } },
    );
    rerender({ id: "m1", text: "Hello world. Second", streaming: true });
    act(() => vi.advanceTimersByTime(600));
    expect(result.current.trim()).toBe("Hello world.");
  });

  it("emits only the new sentence as the next delta", () => {
    const { result, rerender } = renderHook(
      ({ id, text, streaming }) => useStreamAnnouncer(id, text, streaming),
      { initialProps: { id: "m1", text: "One. Two", streaming: true } },
    );
    act(() => vi.advanceTimersByTime(600));
    expect(result.current.trim()).toBe("One.");
    rerender({ id: "m1", text: "One. Two.", streaming: true });
    act(() => vi.advanceTimersByTime(600));
    expect(result.current.trim()).toBe("Two.");
  });

  it("flushes the tail when streaming stops", () => {
    const { result, rerender } = renderHook(
      ({ id, text, streaming }) => useStreamAnnouncer(id, text, streaming),
      { initialProps: { id: "m1", text: "One. Tail", streaming: true } },
    );
    act(() => vi.advanceTimersByTime(600));
    expect(result.current.trim()).toBe("One.");
    rerender({ id: "m1", text: "One. Tail", streaming: false });
    act(() => vi.advanceTimersByTime(0));
    expect(result.current).toContain("Tail");
  });

  it("resets the offset when the message id changes", () => {
    const { result, rerender } = renderHook(
      ({ id, text, streaming }) => useStreamAnnouncer(id, text, streaming),
      { initialProps: { id: "m1", text: "One.", streaming: true } },
    );
    act(() => vi.advanceTimersByTime(600));
    expect(result.current.trim()).toBe("One.");
    // New turn: same leading text must be announced again from the start.
    rerender({ id: "m2", text: "One.", streaming: true });
    act(() => vi.advanceTimersByTime(600));
    expect(result.current.trim()).toBe("One.");
  });
});
