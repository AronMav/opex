"use client";

// ── use-scroll-to-message.ts ─────────────────────────────────────────────────
// Branch-aware jump-to-message. Consumes palette-store.target (set by the
// search palette in Task 4, or the scroll-restore machinery in Task 13c) and
// makes the target message visible:
//
//   1. Stream active → refuse (toast), clear target.
//   2. Target row is loaded → switch to the branch that contains it (walk up
//      parent_message_id), raise renderLimit so it enters the virtualised
//      window, scroll to it and flash a highlight.
//   3. Target row not loaded → page older history (loadPreviousMessages) up to
//      MAX_BACKFILL_PAGES times, re-checking each time. Exhausted / no more
//      history → toast (message too far back or deleted).
//   4. silent target → no toast, no highlight; any failure is swallowed.
//
// Re-entrancy: a fresh target supersedes an in-flight resolution via a
// generation counter. The effect that starts a resolution keys ONLY on the
// target/session/agent — history pages arriving for other reasons never
// re-trigger it.

import { useCallback, useEffect, useRef } from "react";
import { toast } from "sonner";
import { useChatStore, isActivePhase } from "@/stores/chat-store";
import { getCachedRawMessages, resolveActivePath } from "@/stores/chat-history";
import { usePaletteStore } from "@/stores/palette-store";
import { useTranslation } from "@/hooks/use-translation";
import type { ChatMessage } from "@/stores/chat-types";
import type { MessageRow } from "@/types/api";
import { scrollToMessageIndex } from "../message-list-handle";

const HIGHLIGHT_MS = 2000;
const MAX_BACKFILL_PAGES = 20;

/**
 * Walk from `id` up to the root, recording the parentId → childId choice at each
 * step. Applying these picks to `selectedBranches` puts every fork on the path
 * onto the branch that leads to `id` (non-fork entries are inert for
 * resolveActivePath, which only consults selectedBranches where a parent has
 * multiple children).
 */
function pathToRoot(rows: MessageRow[], id: string): Map<string, string> {
  const byId = new Map(rows.map((r) => [r.id, r]));
  const picks = new Map<string, string>();
  let cur = byId.get(id);
  while (cur?.parent_message_id) {
    picks.set(cur.parent_message_id, cur.id);
    cur = byId.get(cur.parent_message_id);
  }
  return picks;
}

type Pending = { messageId: string; silent: boolean };

export function useScrollToMessage(
  agent: string,
  activeSessionId: string | null,
  /** The array Virtuoso actually renders (ChatThread's `allMessages`) — the
   *  scroll index MUST be computed against this, not the raw row order. */
  messages: ChatMessage[],
): void {
  const { t } = useTranslation();
  const target = usePaletteStore((s) => s.target);
  const setTarget = usePaletteStore((s) => s.setTarget);
  const setHighlighted = usePaletteStore((s) => s.setHighlighted);

  // Latest rendered array for the (post-re-render) scroll step — a ref avoids a
  // stale closure inside the async resolution loop.
  const messagesRef = useRef(messages);
  messagesRef.current = messages;

  const genRef = useRef(0);
  const pendingRef = useRef<Pending | null>(null);
  const highlightTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Scroll to the pending target if it is present in the CURRENT rendered array
  // (either already visible, or newly visible after a branch switch / renderLimit
  // bump re-render). Clears the pending slot on success.
  const tryScroll = useCallback(() => {
    const pending = pendingRef.current;
    if (!pending) return;
    const idx = messagesRef.current.findIndex(
      (m) => m.id === pending.messageId || !!m.mergedIds?.includes(pending.messageId),
    );
    if (idx < 0) return;
    scrollToMessageIndex(idx);
    if (!pending.silent) {
      setHighlighted(pending.messageId);
      if (highlightTimerRef.current) clearTimeout(highlightTimerRef.current);
      highlightTimerRef.current = setTimeout(() => setHighlighted(null), HIGHLIGHT_MS);
    }
    pendingRef.current = null;
  }, [setHighlighted]);

  // ── Resolution: react to a newly-set target ────────────────────────────────
  useEffect(() => {
    if (!target?.messageId) return;
    if (activeSessionId == null || target.sessionId !== activeSessionId) return;

    const messageId = target.messageId;
    const silent = target.silent ?? false;
    const gen = ++genRef.current;

    // 1. Refuse while a turn is in flight — switching branches / re-resolving the
    //    active path mid-stream would blend two unrelated branch lineages.
    if (isActivePhase(useChatStore.getState().agents[agent]?.connectionPhase)) {
      if (!silent) toast.error(t("palette.blocked_streaming"));
      setTarget(null);
      return;
    }

    pendingRef.current = { messageId, silent };

    // Apply branch picks + raise renderLimit for the resolved target, then clear
    // the target and attempt an immediate scroll (covers the already-visible
    // case where no re-render is triggered).
    const finish = (rows: MessageRow[]) => {
      resolveInPlace(agent, rows, messageId);
      setTarget(null);
      tryScroll();
    };

    // Fast path: target already loaded.
    const rows = getCachedRawMessages(activeSessionId, agent);
    if (rows.some((r) => r.id === messageId)) {
      finish(rows);
      return;
    }

    // Slow path: page older history until it appears (or we exhaust it).
    let cancelled = false;
    const stale = () => cancelled || genRef.current !== gen;
    void (async () => {
      for (let i = 0; i < MAX_BACKFILL_PAGES; i++) {
        if (stale()) return;
        if (!useChatStore.getState().agents[agent]?.hasMoreHistory) break;
        await useChatStore.getState().loadPreviousMessages(agent);
        if (stale()) return;
        const paged = getCachedRawMessages(activeSessionId, agent);
        if (paged.some((r) => r.id === messageId)) {
          finish(paged);
          return;
        }
      }
      if (stale()) return;
      // Exhausted: too far back, or the message was deleted.
      if (!silent) toast.error(t("palette.too_deep"));
      setTarget(null);
      pendingRef.current = null;
    })();

    return () => {
      cancelled = true;
    };
    // messagesRef/tryScroll are refs/stable; `messages` is intentionally NOT a
    // dep — the scroll effect below owns the post-re-render scroll.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target, activeSessionId, agent, setTarget, t]);

  // ── Scroll once the resolved target enters the rendered window ──────────────
  useEffect(() => {
    tryScroll();
  }, [messages, tryScroll]);

  // Clean up a dangling highlight timer on unmount.
  useEffect(
    () => () => {
      if (highlightTimerRef.current) clearTimeout(highlightTimerRef.current);
    },
    [],
  );
}

/**
 * Merge the branch picks that lead to `messageId` into selectedBranches and
 * raise renderLimit to at least the resolved active-path length so the target
 * survives the `filteredMessages.slice(-renderLimit)` window in ChatThread.
 * Direct immer setState (not switchBranch) because we set MULTIPLE picks at once
 * and must skip switchBranch's stream-guard / query-invalidation side effects —
 * mutating selectedBranches already produces a fresh reference that drives the
 * re-resolution (useRenderMessages).
 */
function resolveInPlace(agent: string, rows: MessageRow[], messageId: string): void {
  const picks = pathToRoot(rows, messageId);
  useChatStore.setState((draft) => {
    const st = draft.agents[agent];
    if (!st) return;
    for (const [parentId, childId] of picks) {
      st.selectedBranches[parentId] = childId;
    }
    const pathLen = resolveActivePath(rows, st.selectedBranches).length;
    if (pathLen > st.renderLimit) st.renderLimit = pathLen;
  });
}
