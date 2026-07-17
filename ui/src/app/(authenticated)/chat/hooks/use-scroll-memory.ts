"use client";

// ── use-scroll-memory.ts ─────────────────────────────────────────────────────
// Task 13c: per-session scroll position memory.
//
// Write side (useScrollMemoryWrite, called from MessageList where Virtuoso's
// rangeChanged + useChatAutoscroll's `shouldFollow` both live): persists the
// id of the first visible message, debounced 500ms, ONLY while the user is
// detached from the tail (`!shouldFollow`) — a user who is following new
// output has no "position" worth remembering. Once the user manually returns
// to the bottom (`shouldFollow` flips back to true), the stored entry for
// that session is cleared — there is nothing to restore to anymore.
//
// Restore side (useScrollMemoryRestore, called from ChatThread where
// activeSessionId + connectionPhase already live): on opening a
// NON-streaming session with a stored id, silently sets the palette-store
// target — the SAME jump mechanism the search palette and bookmarks use
// (`useScrollToMessage`, Task 3). `silent: true` means no toast/highlight,
// and a missing/deleted target message id is that hook's job to no-op on
// quietly — this module never inspects whether the id still exists.
//
// Storage: one localStorage key per session (`scroll_pos:{sessionId}`) plus
// an LRU index array (`scroll_pos_index`) capping the number of remembered
// sessions at 50 — writing a 51st distinct session evicts the
// least-recently-written one.

import { useCallback, useEffect, useRef } from "react";
import { usePaletteStore } from "@/stores/palette-store";

const KEY_PREFIX = "scroll_pos:";
const INDEX_KEY = "scroll_pos_index";
const MAX_ENTRIES = 50;
const DEBOUNCE_MS = 500;

// ── localStorage helpers (pure — also exercised directly by tests) ─────────

function loadIndex(): string[] {
  try {
    const raw = localStorage.getItem(INDEX_KEY);
    if (raw) return JSON.parse(raw);
  } catch {
    /* ignore — corrupt/inaccessible storage falls back to an empty index */
  }
  return [];
}

function saveIndex(index: string[]): void {
  try {
    localStorage.setItem(INDEX_KEY, JSON.stringify(index));
  } catch {
    /* ignore — e.g. storage quota exceeded or disabled */
  }
}

export function getStoredScrollPos(sessionId: string): string | null {
  try {
    return localStorage.getItem(KEY_PREFIX + sessionId);
  } catch {
    return null;
  }
}

export function setStoredScrollPos(sessionId: string, messageId: string): void {
  try {
    localStorage.setItem(KEY_PREFIX + sessionId, messageId);
  } catch {
    return; // storage unavailable — don't touch the index either
  }
  // Move sessionId to the most-recently-used end; evict the oldest once the
  // LRU cap is exceeded (both the index entry and its scroll_pos:* key).
  const index = loadIndex().filter((id) => id !== sessionId);
  index.push(sessionId);
  while (index.length > MAX_ENTRIES) {
    const evicted = index.shift();
    if (evicted) {
      try {
        localStorage.removeItem(KEY_PREFIX + evicted);
      } catch {
        /* ignore */
      }
    }
  }
  saveIndex(index);
}

export function clearStoredScrollPos(sessionId: string): void {
  try {
    localStorage.removeItem(KEY_PREFIX + sessionId);
  } catch {
    /* ignore */
  }
  const index = loadIndex().filter((id) => id !== sessionId);
  saveIndex(index);
}

// ── Write: debounced persistence, gated on !shouldFollow ────────────────────

/**
 * Returns a stable callback — wire it onto Virtuoso's `rangeChanged` (via the
 * first-visible message id) in MessageList. Debounces writes 500ms and only
 * writes while `shouldFollow` is false (user detached from the tail). Clears
 * the stored entry as soon as `shouldFollow` flips back to true.
 */
export function useScrollMemoryWrite(
  sessionId: string | null,
  shouldFollow: boolean,
): (messageId: string | null) => void {
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const prevShouldFollowRef = useRef(shouldFollow);

  // Return-to-bottom: clear the stored entry once the user is following again.
  useEffect(() => {
    if (shouldFollow && !prevShouldFollowRef.current && sessionId) {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
      clearStoredScrollPos(sessionId);
    }
    prevShouldFollowRef.current = shouldFollow;
  }, [shouldFollow, sessionId]);

  // Cancel any pending debounced write on unmount / session change.
  useEffect(() => {
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [sessionId]);

  const recordVisibleMessage = useCallback(
    (messageId: string | null) => {
      if (!sessionId || !messageId || shouldFollow) return;
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(() => {
        timerRef.current = null;
        setStoredScrollPos(sessionId, messageId);
      }, DEBOUNCE_MS);
    },
    [sessionId, shouldFollow],
  );

  return recordVisibleMessage;
}

// ── Restore: one-shot silent jump on opening a non-streaming session ────────

/**
 * On mount / session change, if the session is NOT streaming and a scroll
 * position was previously stored for it, silently sets the palette-store
 * target so `useScrollToMessage` (Task 3) jumps there without any toast or
 * highlight. Attempts at most once per session id — a session flipping in
 * and out of `isStreaming` doesn't re-trigger a restore once it has already
 * been attempted (successfully or not).
 *
 * A PENDING jump target always wins over scroll memory: the palette /
 * bookmark flow sets its target BEFORE navigating (setTarget →
 * selectSession/router.push → ChatThread mounts → this effect fires), so an
 * unconditional setTarget here would clobber the user's explicit jump and
 * land them on the remembered scroll position instead of the searched
 * message. If any target is already pending, the restore yields (and the
 * per-session attempt is still consumed — the explicit jump defines where
 * the user is now; re-restoring after it would yank them away).
 */
export function useScrollMemoryRestore(sessionId: string | null, isStreaming: boolean): void {
  const attemptedRef = useRef<string | null>(null);

  useEffect(() => {
    if (!sessionId || isStreaming) return;
    if (attemptedRef.current === sessionId) return;
    attemptedRef.current = sessionId;

    // Yield to a pending palette/bookmark jump — a live target always wins.
    if (usePaletteStore.getState().target) return;

    const storedId = getStoredScrollPos(sessionId);
    if (!storedId) return;
    usePaletteStore.getState().setTarget({ sessionId, messageId: storedId, silent: true });
  }, [sessionId, isStreaming]);
}
