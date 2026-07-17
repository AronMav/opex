/**
 * Wave-2 Task 13c: per-session scroll position memory.
 *
 * Write side (useScrollMemoryWrite): persists the first-visible message id
 * (debounced 500ms) ONLY while the user is detached from the tail
 * (`!shouldFollow`); clears the stored entry once the user returns to the
 * bottom (`shouldFollow` flips back to true). LRU-capped at 50 sessions via
 * the `scroll_pos_index` localStorage array.
 *
 * Restore side (useScrollMemoryRestore): on opening a NON-streaming session
 * with a stored id, silently sets the palette-store target (Task 3's
 * useScrollToMessage consumes it — a missing/deleted id is its job to
 * silently no-op, not this hook's). Never restores while streaming.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

const { setTargetMock, paletteState } = vi.hoisted(() => ({
  setTargetMock: vi.fn(),
  // Mutable holder so tests can stage a PENDING jump target — the restore
  // guard reads usePaletteStore.getState().target before setting its own.
  paletteState: {
    target: null as { sessionId: string; messageId?: string; silent?: boolean } | null,
  },
}));

vi.mock("@/stores/palette-store", () => ({
  usePaletteStore: {
    getState: () => ({ target: paletteState.target, setTarget: setTargetMock }),
  },
}));

import {
  useScrollMemoryWrite,
  useScrollMemoryRestore,
  getStoredScrollPos,
  setStoredScrollPos,
} from "../use-scroll-memory";

const SID = "sess-1";

beforeEach(() => {
  localStorage.clear();
  setTargetMock.mockClear();
  paletteState.target = null;
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useScrollMemoryWrite", () => {
  it("persists the first-visible message id (debounced) only while detached from the tail", () => {
    const { result } = renderHook(
      ({ shouldFollow }: { shouldFollow: boolean }) => useScrollMemoryWrite(SID, shouldFollow),
      { initialProps: { shouldFollow: false } },
    );

    act(() => {
      result.current("m42");
    });
    // Not yet written — debounce window still open.
    expect(getStoredScrollPos(SID)).toBeNull();

    act(() => {
      vi.advanceTimersByTime(500);
    });
    expect(getStoredScrollPos(SID)).toBe("m42");
  });

  it("does NOT write while shouldFollow is true (user at the tail)", () => {
    const { result } = renderHook(() => useScrollMemoryWrite(SID, true));

    act(() => {
      result.current("m42");
      vi.advanceTimersByTime(500);
    });
    expect(getStoredScrollPos(SID)).toBeNull();
  });

  it("coalesces rapid successive calls into a single debounced write of the LATEST id", () => {
    const { result } = renderHook(() => useScrollMemoryWrite(SID, false));

    act(() => {
      result.current("m1");
      vi.advanceTimersByTime(200);
      result.current("m2");
      vi.advanceTimersByTime(200);
      result.current("m3");
    });
    expect(getStoredScrollPos(SID)).toBeNull(); // still within the debounce window

    act(() => {
      vi.advanceTimersByTime(500);
    });
    expect(getStoredScrollPos(SID)).toBe("m3");
  });

  it("clears the stored entry when shouldFollow transitions back to true (return to bottom)", () => {
    setStoredScrollPos(SID, "m42");
    expect(getStoredScrollPos(SID)).toBe("m42");

    const { rerender } = renderHook(
      ({ shouldFollow }: { shouldFollow: boolean }) => useScrollMemoryWrite(SID, shouldFollow),
      { initialProps: { shouldFollow: false } },
    );

    act(() => {
      rerender({ shouldFollow: true });
    });
    expect(getStoredScrollPos(SID)).toBeNull();
  });

  it("evicts the oldest session past the 50-entry LRU cap", () => {
    for (let i = 0; i < 51; i++) {
      setStoredScrollPos(`s${i}`, `m${i}`);
    }
    // The very first session (s0) was evicted.
    expect(getStoredScrollPos("s0")).toBeNull();
    // The most recent 50 remain.
    expect(getStoredScrollPos("s50")).toBe("m50");
    expect(getStoredScrollPos("s1")).toBe("m1");
    const index = JSON.parse(localStorage.getItem("scroll_pos_index") ?? "[]");
    expect(index.length).toBe(50);
  });
});

describe("useScrollMemoryRestore", () => {
  it("silently sets the palette target when opening a non-streaming session with a stored id", () => {
    setStoredScrollPos(SID, "m7");

    renderHook(() => useScrollMemoryRestore(SID, false));

    expect(setTargetMock).toHaveBeenCalledWith({ sessionId: SID, messageId: "m7", silent: true });
  });

  it("does nothing when there is no stored id for the session", () => {
    renderHook(() => useScrollMemoryRestore(SID, false));
    expect(setTargetMock).not.toHaveBeenCalled();
  });

  it("never restores while the session is streaming (isActivePhase)", () => {
    setStoredScrollPos(SID, "m7");

    renderHook(() => useScrollMemoryRestore(SID, true));

    expect(setTargetMock).not.toHaveBeenCalled();
  });

  it("attempts restore only once per session (no repeat calls on unrelated rerenders)", () => {
    setStoredScrollPos(SID, "m7");

    const { rerender } = renderHook(
      ({ isStreaming }: { isStreaming: boolean }) => useScrollMemoryRestore(SID, isStreaming),
      { initialProps: { isStreaming: false } },
    );
    expect(setTargetMock).toHaveBeenCalledTimes(1);

    rerender({ isStreaming: false });
    expect(setTargetMock).toHaveBeenCalledTimes(1);
  });

  it("yields to a PENDING palette/bookmark jump target (does not clobber it with the stored position)", () => {
    // Cross-agent palette flow: SearchPalette sets a real (non-silent) target,
    // then navigates; ChatThread mounts and this restore effect fires with a
    // stored scroll position for the same session. The explicit jump must win.
    setStoredScrollPos(SID, "m7");
    paletteState.target = { sessionId: SID, messageId: "searched-msg" };

    renderHook(() => useScrollMemoryRestore(SID, false));

    // Restore yielded: setTarget was never called, the pending target is intact.
    expect(setTargetMock).not.toHaveBeenCalled();
    expect(paletteState.target).toEqual({ sessionId: SID, messageId: "searched-msg" });
  });
});
