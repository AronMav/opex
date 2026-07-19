"use client";

import { useState, useRef, useEffect, useCallback } from "react";
import { type VirtuosoHandle } from "react-virtuoso";
import { attachTailSentinel } from "./tail-sentinel";
import { runScrollToBottom } from "./scroll-to-bottom";

/**
 * Encapsulated auto-follow and tail-detection logic for the chat message list.
 * Fixes performance (O(1) token tracking), rAF conflicts, and inertia-lock bugs.
 */
export function useChatAutoscroll(
  isStreaming: boolean,
  activeSessionId: string | null
) {
  const virtuosoRef = useRef<VirtuosoHandle>(null);

  // Geometric state: is the viewport physically at the bottom?
  const [isAtTail, setIsAtTail] = useState(true);
  const isAtTailRef = useRef(true);

  // Intent state: should the viewport follow new content?
  const [shouldFollow, setShouldFollow] = useState(true);
  const shouldFollowRef = useRef(true);

  // Badge state: how many tokens arrived while user was away?
  const [missedTokens, setMissedTokens] = useState(0);
  const missedTokensRef = useRef(0);

  // DOM elements (exposed via state to trigger effect re-attach on remount)
  const [sentinelEl, setSentinelEl] = useState<HTMLDivElement | null>(null);
  const [scrollerEl, setScrollerEl] = useState<HTMLElement | null>(null);

  // Performance: track text growth for the tail message ONLY (O(1)). H1 fix:
  // the badge previously counted `parts.length` which stays at 1 for an
  // entire streaming text reply (the text part is mutated in place) — the
  // badge gave zero signal during long answers. Track the total text length
  // instead so the user sees real growth.
  const lastMsgTextLenRef = useRef(0);
  const lastMsgIdRef = useRef<string | null>(null);

  // Reset badge when user reaches the tail
  useEffect(() => {
    if (isAtTail) {
      missedTokensRef.current = 0;
      setMissedTokens(0);
    }
  }, [isAtTail]);

  // IntersectionObserver: sentinel (in Footer) -> isAtTail
  useEffect(() => {
    if (!scrollerEl || !sentinelEl) return;

    return attachTailSentinel(scrollerEl, sentinelEl, (atTail) => {
      isAtTailRef.current = atTail;
      setIsAtTail(atTail);

      // Auto-restore follow intent when user manually reaches the bottom
      if (atTail && !shouldFollowRef.current) {
        shouldFollowRef.current = true;
        setShouldFollow(true);
      }
    });
  }, [scrollerEl, sentinelEl]);

  // H2 fix: the continuous 60Hz rAF scroll-pin loop was removed. It forced
  // synchronous layout reads/writes every frame for the entire duration of
  // streaming regardless of whether new content arrived — the dominant cause
  // of "chat gets sluggish during long responses". `react-virtuoso`'s
  // `followOutput="auto"` (configured on the Virtuoso instance in MessageList)
  // already pins to the bottom as new rows arrive, with internally throttled
  // layout work that only fires on actual content changes. The user-intent
  // detection (wheel/touch/key) above still flips `shouldFollow` off so the
  // Virtuoso followOutput gate stays correct.

  // User-intent detection: wheel/touch/key events flip shouldFollow OFF.
  useEffect(() => {
    if (!scrollerEl) return;
    let touchStartY = 0;

    const turnOff = () => {
      if (shouldFollowRef.current) {
        shouldFollowRef.current = false;
        setShouldFollow(false);
      }
    };

    const onWheel = (e: WheelEvent) => {
      // Threshold -20 avoids small jitters/inertia tails from killing follow intent.
      if (e.deltaY < -20) turnOff();
    };
    const onTouchStart = (e: TouchEvent) => {
      touchStartY = e.touches[0]?.clientY ?? 0;
    };
    const onTouchMove = (e: TouchEvent) => {
      const dy = (e.touches[0]?.clientY ?? 0) - touchStartY;
      if (dy > 25) turnOff(); // User pulled content DOWN (scrolled UP)
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (["PageUp", "ArrowUp", "Home"].includes(e.key)) turnOff();
    };

    scrollerEl.addEventListener("wheel", onWheel, { passive: true });
    scrollerEl.addEventListener("touchstart", onTouchStart, { passive: true });
    scrollerEl.addEventListener("touchmove", onTouchMove, { passive: true });
    scrollerEl.addEventListener("keydown", onKeyDown);

    return () => {
      scrollerEl.removeEventListener("wheel", onWheel);
      scrollerEl.removeEventListener("touchstart", onTouchStart);
      scrollerEl.removeEventListener("touchmove", onTouchMove);
      scrollerEl.removeEventListener("keydown", onKeyDown);
    };
  }, [scrollerEl]);

  // Reset follow intent on session switch
  const prevSessionId = useRef(activeSessionId);
  useEffect(() => {
    if (activeSessionId !== prevSessionId.current) {
      prevSessionId.current = activeSessionId;
      shouldFollowRef.current = true;
      setShouldFollow(true);
      // Wait for session history to load/render before anchoring
      const t = setTimeout(() => {
        if (scrollerEl) scrollerEl.scrollTop = scrollerEl.scrollHeight;
      }, 100);
      return () => clearTimeout(t);
    }
  }, [activeSessionId, scrollerEl]);

  const scrollToBottom = useCallback(() => {
    shouldFollowRef.current = true;
    setShouldFollow(true);
    runScrollToBottom(virtuosoRef.current);
    // UI response: clear badge immediately
    setMissedTokens(0);
    missedTokensRef.current = 0;
  }, []);

  const trackNewTokens = useCallback((lastMsgId: string, textLen: number) => {
    if (!isAtTailRef.current) {
      if (lastMsgId === lastMsgIdRef.current) {
        const delta = Math.max(0, textLen - lastMsgTextLenRef.current);
        missedTokensRef.current += delta;
      } else {
        // New message started while away from tail
        missedTokensRef.current += textLen;
      }
      setMissedTokens(missedTokensRef.current);
    }
    lastMsgIdRef.current = lastMsgId;
    lastMsgTextLenRef.current = textLen;
  }, []);

  return {
    virtuosoRef,
    setSentinelEl,
    setScrollerEl,
    isAtTail,
    shouldFollow,
    missedTokens,
    scrollToBottom,
    trackNewTokens,
  };
}
