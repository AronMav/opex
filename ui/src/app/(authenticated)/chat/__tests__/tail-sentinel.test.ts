/**
 * Unit tests for the tail-sentinel IntersectionObserver wrapper.
 *
 * IntersectionObserver has no native jsdom implementation in vitest.
 * We install a hand-written mock via `vi.stubGlobal` per-test so that
 * construction options, target element, and callback plumbing are all
 * directly observable. Production code uses the real browser IO — the
 * wrapper itself is a thin adapter.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  attachTailSentinel,
  DEFAULT_TAIL_ROOT_MARGIN,
} from "../tail-sentinel";
import { MockIntersectionObserver } from "./mock-intersection-observer";

beforeEach(() => {
  MockIntersectionObserver.instances = [];
  vi.stubGlobal("IntersectionObserver", MockIntersectionObserver);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("attachTailSentinel", () => {
  it("exports a 200px bottom rootMargin constant", () => {
    expect(DEFAULT_TAIL_ROOT_MARGIN).toBe("200px 0px");
  });

  it("constructs IO with the scroller as root and 200px rootMargin", () => {
    const scroller = document.createElement("div");
    const sentinel = document.createElement("div");
    attachTailSentinel(scroller, sentinel, () => {});

    const io = MockIntersectionObserver.last();
    expect(io.options?.root).toBe(scroller);
    expect(io.options?.rootMargin).toBe(DEFAULT_TAIL_ROOT_MARGIN);
  });

  it("observes the sentinel element", () => {
    const scroller = document.createElement("div");
    const sentinel = document.createElement("div");
    attachTailSentinel(scroller, sentinel, () => {});

    expect(MockIntersectionObserver.last().observed).toEqual([sentinel]);
  });

  it("invokes the callback with true when sentinel enters the viewport", () => {
    const scroller = document.createElement("div");
    const sentinel = document.createElement("div");
    const cb = vi.fn();
    attachTailSentinel(scroller, sentinel, cb);

    MockIntersectionObserver.last().fire(true);
    expect(cb).toHaveBeenCalledExactlyOnceWith(true);
  });

  it("invokes the callback with false when sentinel leaves the viewport", () => {
    const scroller = document.createElement("div");
    const sentinel = document.createElement("div");
    const cb = vi.fn();
    attachTailSentinel(scroller, sentinel, cb);

    MockIntersectionObserver.last().fire(false);
    expect(cb).toHaveBeenCalledExactlyOnceWith(false);
  });

  it("propagates multiple transitions in order", () => {
    const scroller = document.createElement("div");
    const sentinel = document.createElement("div");
    const cb = vi.fn();
    attachTailSentinel(scroller, sentinel, cb);

    const io = MockIntersectionObserver.last();
    io.fire(true);
    io.fire(false);
    io.fire(true);

    expect(cb.mock.calls).toEqual([[true], [false], [true]]);
  });

  it("returns a teardown that disconnects the observer", () => {
    const scroller = document.createElement("div");
    const sentinel = document.createElement("div");
    const detach = attachTailSentinel(scroller, sentinel, () => {});

    expect(MockIntersectionObserver.last().disconnected).toBe(false);
    detach();
    expect(MockIntersectionObserver.last().disconnected).toBe(true);
  });

  it("accepts an optional rootMargin override", () => {
    const scroller = document.createElement("div");
    const sentinel = document.createElement("div");
    attachTailSentinel(scroller, sentinel, () => {}, {
      rootMargin: "50px 0px",
    });

    expect(MockIntersectionObserver.last().options?.rootMargin).toBe("50px 0px");
  });

  it("does not invoke the callback when the observer fires with an empty batch", () => {
    const scroller = document.createElement("div");
    const sentinel = document.createElement("div");
    const cb = vi.fn();
    attachTailSentinel(scroller, sentinel, cb);

    // Simulate the browser firing with no entries (pathological but
    // well-defined per spec). The `if (entry)` guard must short-circuit.
    const io = MockIntersectionObserver.last();
    io.callback([], io as unknown as IntersectionObserver);

    expect(cb).not.toHaveBeenCalled();
  });
});
