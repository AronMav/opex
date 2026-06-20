"use client";

import { useRef, useCallback, type TouchEvent } from "react";

export interface SwipeHandlers {
  onTouchStart: (e: TouchEvent) => void;
  onTouchMove: (e: TouchEvent) => void;
  onTouchEnd: () => void;
}

export interface SwipeCallbacks {
  onSwipeLeft?: () => void;
  onSwipeRight?: () => void;
  threshold?: number;
}

/**
 * Lightweight swipe gesture detector for touch devices.
 * Tracks horizontal swipe and fires callbacks when threshold is exceeded.
 * Returns touch handlers to spread onto the target element.
 */
export function useSwipeGesture({ onSwipeLeft, onSwipeRight, threshold = 80 }: SwipeCallbacks): SwipeHandlers {
  const startXRef = useRef(0);
  const startYRef = useRef(0);
  const currentXRef = useRef(0);
  const trackingRef = useRef(false);

  const onTouchStart = useCallback((e: TouchEvent) => {
    const touch = e.touches[0];
    if (!touch) return;
    startXRef.current = touch.clientX;
    startYRef.current = touch.clientY;
    currentXRef.current = touch.clientX;
    trackingRef.current = true;
  }, []);

  const onTouchMove = useCallback((e: TouchEvent) => {
    if (!trackingRef.current) return;
    const touch = e.touches[0];
    if (!touch) return;
    currentXRef.current = touch.clientX;

    // Cancel tracking if vertical movement exceeds horizontal
    const dx = Math.abs(touch.clientX - startXRef.current);
    const dy = Math.abs(touch.clientY - startYRef.current);
    if (dy > dx * 1.5) {
      trackingRef.current = false;
    }
  }, []);

  const onTouchEnd = useCallback(() => {
    if (!trackingRef.current) return;
    trackingRef.current = false;

    const dx = currentXRef.current - startXRef.current;
    if (Math.abs(dx) >= threshold) {
      if (dx < 0 && onSwipeLeft) {
        onSwipeLeft();
      } else if (dx > 0 && onSwipeRight) {
        onSwipeRight();
      }
    }
  }, [onSwipeLeft, onSwipeRight, threshold]);

  return { onTouchStart, onTouchMove, onTouchEnd };
}
