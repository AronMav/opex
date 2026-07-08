"use client";

import { useEffect, type KeyboardEvent, type RefObject } from "react";

const TABBABLE =
  'button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Focus management for custom (non-Radix) overlays. When `active` becomes true,
 * moves focus into `containerRef` (the first tabbable, or the container itself
 * when `initialFocus === "container"`); when it becomes false again, restores
 * focus to `restoreTo`. The returned handler, spread onto the container's
 * `onKeyDown`, keeps Tab / Shift+Tab cycling WITHIN the container — including the
 * edge cases where focus sits on the container itself or has escaped entirely
 * (both pull it back in), which a naive `active === first/last` check misses.
 */
export function useFocusTrap({
  active,
  containerRef,
  restoreTo,
  initialFocus = "first",
}: {
  active: boolean;
  containerRef: RefObject<HTMLElement | null>;
  restoreTo: RefObject<HTMLElement | null>;
  initialFocus?: "first" | "container";
}): (e: KeyboardEvent) => void {
  useEffect(() => {
    if (!active) return;
    const container = containerRef.current;
    if (!container) return;
    // Capture the restore target at open time (it's the stable trigger element);
    // reading the ref in cleanup would trip react-hooks/exhaustive-deps.
    const restoreTarget = restoreTo.current;
    if (initialFocus === "container") {
      container.focus();
    } else {
      const first = container.querySelector<HTMLElement>(TABBABLE);
      (first ?? container).focus();
    }
    return () => {
      restoreTarget?.focus();
    };
  }, [active, containerRef, restoreTo, initialFocus]);

  return (e: KeyboardEvent) => {
    if (e.key !== "Tab") return;
    const container = containerRef.current;
    if (!container) return;
    const tabbables = Array.from(container.querySelectorAll<HTMLElement>(TABBABLE));
    if (tabbables.length === 0) {
      e.preventDefault();
      container.focus();
      return;
    }
    const first = tabbables[0];
    const last = tabbables[tabbables.length - 1];
    const activeEl = document.activeElement;
    const insideChild = container.contains(activeEl) && activeEl !== container;
    if (e.shiftKey) {
      if (!insideChild || activeEl === first) {
        e.preventDefault();
        last.focus();
      }
    } else if (!insideChild || activeEl === last) {
      e.preventDefault();
      first.focus();
    }
  };
}
