/**
 * Harness-based integration test for the sentinel → React state flow
 * used inside MessageList. Avoids mocking Virtuoso + chat store +
 * session router (which would dwarf the code under test).
 *
 * The Harness reproduces the same useEffect pattern MessageList uses:
 *   - lookup scroller + sentinel from the DOM
 *   - attachTailSentinel
 *   - forward callback to setState + ref
 *   - teardown on unmount
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, act } from "@testing-library/react";
import React, { useRef, useState, useEffect } from "react";
import { attachTailSentinel } from "../tail-sentinel";
import { MockIntersectionObserver } from "./mock-intersection-observer";

beforeEach(() => {
  MockIntersectionObserver.instances = [];
  vi.stubGlobal("IntersectionObserver", MockIntersectionObserver);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Minimal reproduction of the MessageList attach pattern. */
function Harness({ onState }: { onState: (s: { isAtTail: boolean; ref: boolean }) => void }) {
  const scrollerRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);
  const isAtTailRef = useRef<boolean>(true);
  const [isAtTail, setIsAtTail] = useState<boolean>(true);

  useEffect(() => {
    const scroller = scrollerRef.current;
    const sentinel = sentinelRef.current;
    if (!scroller || !sentinel) return;

    const detach = attachTailSentinel(scroller, sentinel, (atTail) => {
      isAtTailRef.current = atTail;
      setIsAtTail((prev) => (prev === atTail ? prev : atTail));
    });
    return detach;
  }, []);

  onState({ isAtTail, ref: isAtTailRef.current });

  return (
    <div ref={scrollerRef}>
      <div ref={sentinelRef} data-testid="sentinel" />
    </div>
  );
}

describe("tail-sentinel integration — Harness", () => {
  it("initial state is isAtTail=true", () => {
    let last: { isAtTail: boolean; ref: boolean } | null = null;
    render(<Harness onState={(s) => { last = s; }} />);
    expect(last!.isAtTail).toBe(true);
    expect(last!.ref).toBe(true);
  });

  it("attaches IO with the scroller as root and observes the sentinel", () => {
    render(<Harness onState={() => {}} />);
    const io = MockIntersectionObserver.last();
    expect(io.options?.root).toBeInstanceOf(HTMLDivElement);
    expect(io.observed).toHaveLength(1);
    expect((io.observed[0] as HTMLElement).dataset.testid).toBe("sentinel");
  });

  it("IO false transition flips isAtTail → false (both state and ref)", () => {
    let last: { isAtTail: boolean; ref: boolean } | null = null;
    render(<Harness onState={(s) => { last = s; }} />);

    act(() => {
      MockIntersectionObserver.last().fire(false);
    });

    expect(last!.isAtTail).toBe(false);
    expect(last!.ref).toBe(false);
  });

  it("IO true transition after false restores isAtTail → true", () => {
    let last: { isAtTail: boolean; ref: boolean } | null = null;
    render(<Harness onState={(s) => { last = s; }} />);

    act(() => { MockIntersectionObserver.last().fire(false); });
    act(() => { MockIntersectionObserver.last().fire(true); });

    expect(last!.isAtTail).toBe(true);
    expect(last!.ref).toBe(true);
  });

  it("unmount disconnects the observer", () => {
    const { unmount } = render(<Harness onState={() => {}} />);
    const io = MockIntersectionObserver.last();
    expect(io.disconnected).toBe(false);
    unmount();
    expect(io.disconnected).toBe(true);
  });

  it("duplicate true callbacks do not cause extra re-renders (state dedupes)", () => {
    let renderCount = 0;
    render(
      <Harness
        onState={() => {
          renderCount += 1;
        }}
      />,
    );
    const before = renderCount;

    act(() => { MockIntersectionObserver.last().fire(true); });
    act(() => { MockIntersectionObserver.last().fire(true); });

    // The state setter returns prev unchanged; React skips the re-render.
    expect(renderCount - before).toBeLessThanOrEqual(0);
  });

  it("re-attaches IO when the sentinel DOM node is replaced (Footer remount)", () => {
    // Reproduce the Virtuoso-remount hazard: if the sentinel element
    // identity changes, the effect must tear down the old observer
    // and attach a new one to the fresh node.
    function RemountHarness() {
      const scrollerRef = useRef<HTMLDivElement>(null);
      const [sentinelEl, setSentinelEl] = useState<HTMLDivElement | null>(null);
      const [sentinelKey, setSentinelKey] = useState(0);

      useEffect(() => {
        const scroller = scrollerRef.current;
        if (!scroller || !sentinelEl) return;
        const detach = attachTailSentinel(scroller, sentinelEl, () => {});
        return detach;
      }, [sentinelEl]);

      return (
        <div ref={scrollerRef}>
          <div
            key={sentinelKey}
            ref={setSentinelEl}
            data-testid={`sentinel-${sentinelKey}`}
          />
          <button onClick={() => setSentinelKey((k) => k + 1)}>remount</button>
        </div>
      );
    }

    const { getByText } = render(<RemountHarness />);
    const firstInstanceCount = MockIntersectionObserver.instances.length;

    act(() => { getByText("remount").click(); });

    expect(MockIntersectionObserver.instances.length).toBe(
      firstInstanceCount + 1,
    );
    // Previous observer must be disconnected on teardown
    expect(MockIntersectionObserver.instances[firstInstanceCount - 1].disconnected).toBe(true);
  });

  it("missed-token counter resets when isAtTail transitions false → true", () => {
    // Prove the `useEffect([isAtTail])` reset path: simulate a token
    // accrual while the user is away from the tail, then re-enter and
    // confirm the counter is cleared.
    function MissedTokensHarness({
      bumpRef,
      onBadge,
    }: {
      bumpRef: React.MutableRefObject<(() => void) | null>;
      onBadge: (value: number) => void;
    }) {
      const scrollerRef = useRef<HTMLDivElement>(null);
      const [sentinelEl, setSentinelEl] = useState<HTMLDivElement | null>(null);
      const [isAtTail, setIsAtTail] = useState(true);
      const [missed, setMissed] = useState(0);

      useEffect(() => {
        const scroller = scrollerRef.current;
        if (!scroller || !sentinelEl) return;
        const detach = attachTailSentinel(scroller, sentinelEl, (atTail) => {
          setIsAtTail((prev) => (prev === atTail ? prev : atTail));
        });
        return detach;
      }, [sentinelEl]);

      useEffect(() => {
        if (isAtTail) {
          setMissed(0);
        }
      }, [isAtTail]);

      // Expose a bump function the test can call while `isAtTail` is false.
      bumpRef.current = () => setMissed((m) => m + 1);

      onBadge(missed);
      return (
        <div ref={scrollerRef}>
          <div ref={setSentinelEl} />
        </div>
      );
    }

    const bumpRef: React.MutableRefObject<(() => void) | null> = { current: null };
    let lastBadge = -1;
    render(
      <MissedTokensHarness
        bumpRef={bumpRef}
        onBadge={(v) => { lastBadge = v; }}
      />,
    );
    // Initial mount reset: badge starts at 0 (isAtTail=true triggers reset effect).
    expect(lastBadge).toBe(0);

    // User leaves tail.
    act(() => { MockIntersectionObserver.last().fire(false); });
    expect(lastBadge).toBe(0);

    // Three tokens arrive while away — badge accrues.
    act(() => { bumpRef.current!(); });
    act(() => { bumpRef.current!(); });
    act(() => { bumpRef.current!(); });
    expect(lastBadge).toBe(3);

    // User re-enters tail — reset effect must fire and zero the badge.
    // Without the `useEffect([isAtTail])` reset, this would stay at 3.
    act(() => { MockIntersectionObserver.last().fire(true); });
    expect(lastBadge).toBe(0);
  });

  it("isAtTail=true restores shouldFollow after external force-off", () => {
    // Harness reflects the new behavior: shouldFollow is turned OFF
    // by input events (tested via direct state manipulation here),
    // and turned ON by the sentinel returning to the tail zone.
    function RestoreHarness({
      onState,
      forceOffRef,
    }: {
      onState: (s: { shouldFollow: boolean }) => void;
      forceOffRef: React.MutableRefObject<(() => void) | null>;
    }) {
      const scrollerRef = useRef<HTMLDivElement>(null);
      const [sentinelEl, setSentinelEl] = useState<HTMLDivElement | null>(null);
      const [isAtTail, setIsAtTail] = useState(true);
      const [shouldFollow, setShouldFollow] = useState(true);
      const shouldFollowRef = useRef(true);

      useEffect(() => {
        const scroller = scrollerRef.current;
        if (!scroller || !sentinelEl) return;
        const detach = attachTailSentinel(scroller, sentinelEl, (atTail) => {
          setIsAtTail((prev) => (prev === atTail ? prev : atTail));
        });
        return detach;
      }, [sentinelEl]);

      // Restoration effect: isAtTail=true → shouldFollow=true.
      useEffect(() => {
        if (isAtTail && !shouldFollowRef.current) {
          shouldFollowRef.current = true;
          setShouldFollow(true);
        }
      }, [isAtTail]);

      // Expose a way for the test to force shouldFollow OFF
      // (simulating an input event firing).
      forceOffRef.current = () => {
        shouldFollowRef.current = false;
        setShouldFollow(false);
      };

      onState({ shouldFollow });
      return (
        <div ref={scrollerRef}>
          <div ref={setSentinelEl} />
        </div>
      );
    }

    const forceOffRef: React.MutableRefObject<(() => void) | null> = { current: null };
    let last: { shouldFollow: boolean } = { shouldFollow: true };
    render(
      <RestoreHarness
        onState={(s) => { last = s; }}
        forceOffRef={forceOffRef}
      />,
    );
    expect(last.shouldFollow).toBe(true);

    // User input event (simulated) forces shouldFollow OFF.
    act(() => {
      MockIntersectionObserver.last().fire(false);
      forceOffRef.current!();
    });
    expect(last.shouldFollow).toBe(false);

    // User scrolls back to tail — sentinel re-enters viewport.
    act(() => { MockIntersectionObserver.last().fire(true); });
    expect(last.shouldFollow).toBe(true);
  });
});
