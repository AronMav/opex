"use client";

// ── use-scroll-to-message.ts ─────────────────────────────────────────────────
// Branch-aware jump-to-message. Consumes palette-store.target (set by the
// search palette in Task 4, or the scroll-restore machinery in Task 13c) and
// makes the target message visible:
//
//   1. Stream active → refuse (toast), clear target. Re-checked immediately
//      before a resolved target's branch picks are applied, too — a turn may
//      have started during the (possibly multi-page) backfill await below.
//   2. Target row is loaded → switch to the branch that contains it (walk up
//      parent_message_id), raise renderLimit so it enters the virtualised
//      window, scroll to it and flash a highlight.
//   3. Target row not loaded → page older history directly into the React
//      Query cache (NOT via the `loadPreviousMessages` store action, which
//      reads live-mode messages and no-ops in `mode:"history"` — the mode the
//      palette flow itself enters) up to MAX_BACKFILL_PAGES times, re-checking
//      each time. Exhausted (short page) / fetch failure → toast (message too
//      far back or deleted).
//   4. silent target → no toast, no highlight; any failure is swallowed.
//
// Re-entrancy: a fresh target supersedes an in-flight resolution via a
// generation counter. The effect that starts a resolution keys ONLY on the
// target/session/agent — history pages arriving for other reasons never
// re-trigger it.

import { useCallback, useEffect, useRef } from "react";
import { toast } from "sonner";
import { useChatStore, isActivePhase } from "@/stores/chat-store";
import { getCachedRawMessages, prependOlderRawMessages, resolveActivePath } from "@/stores/chat-history";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";
import { usePaletteStore } from "@/stores/palette-store";
import { useTranslation } from "@/hooks/use-translation";
import { apiGet } from "@/lib/api";
import type { ChatMessage } from "@/stores/chat-types";
import type { MessageRow, MessagesResponse } from "@/types/api";
import { scrollToMessageIndex } from "../message-list-handle";

const HIGHLIGHT_MS = 2000;
const MAX_BACKFILL_PAGES = 20;
const BACKFILL_PAGE_SIZE = 100;
// Bounded window after a resolution to let the resolved target enter the
// rendered window (branch-switch / renderLimit re-render). If the row never
// appears — e.g. a concurrent refetch dropped it from the render array — the
// still-set pendingRef is cleared so no surprise delayed scroll fires later.
const SETTLE_MS = 3000;

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
  /** Whether the active session's first history page has landed in the RQ
   *  cache (ChatThread passes `!!sessionMessagesData`). On a cold cache
   *  selectSession flips `activeSessionId` synchronously, so this effect would
   *  otherwise fire before page 1 is fetched — `getCachedRawMessages` returns
   *  `[]`, there is no anchor row to backfill from, and the target would be
   *  falsely exhausted. While `!historyReady` the resolution is deferred
   *  WITHOUT consuming the target or counting an attempt. */
  historyReady: boolean,
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
  const settleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

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
    // The deferred scroll landed — cancel the resilience timer armed by finish().
    if (settleTimerRef.current) {
      clearTimeout(settleTimerRef.current);
      settleTimerRef.current = null;
    }
  }, [setHighlighted]);

  // ── Resolution: react to a newly-set target ────────────────────────────────
  useEffect(() => {
    if (!target?.messageId) return;
    if (activeSessionId == null || target.sessionId !== activeSessionId) return;
    // C1: defer until the session's first history page is in the cache. On a
    // cold cache selectSession sets activeSessionId synchronously and this
    // effect fires before useSessionMessages fetched page 1, so
    // getCachedRawMessages() would be [] — no anchor to page older history
    // from — falsely exhausting the target. `historyReady` is a dep, so this
    // re-runs (target still set, no attempt spent) once page 1 lands.
    if (!historyReady) return;

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

    // Tracks whether this resolution reached a final decision (finish() or the
    // exhaustion branch below) before the effect got torn down. `finish()` and
    // the exhaustion branch both call `setTarget(null)`, which — since `target`
    // is a dependency — makes THIS SAME resolution's own cleanup fire on the
    // next render; that must NOT wipe the pendingRef it just (deliberately)
    // left set for the `messages`-effect to consume on a later re-render. Only
    // a cleanup that fires WITHOUT a settled resolution (superseded by a fresh
    // target, or the session/agent changed) indicates an abandoned resolution
    // whose pendingRef would otherwise dangle — see the cleanup below.
    let settled = false;

    // Apply branch picks + raise renderLimit for the resolved target, then clear
    // the target and attempt an immediate scroll (covers the already-visible
    // case where no re-render is triggered). Re-checks the stream phase: up to
    // MAX_BACKFILL_PAGES awaits happened between the guard above and here, and a
    // turn may have started in the meantime — blending its branch into a
    // resolution kicked off before it started would corrupt selectedBranches.
    const finish = (rows: MessageRow[]) => {
      settled = true;
      if (isActivePhase(useChatStore.getState().agents[agent]?.connectionPhase)) {
        if (!silent) toast.error(t("palette.blocked_streaming"));
        setTarget(null);
        pendingRef.current = null;
        return;
      }
      resolveInPlace(agent, rows, messageId);
      setTarget(null);
      tryScroll();
      // If the immediate scroll didn't land, the target is deferred to the
      // `messages`-effect (fires after the branch-switch/renderLimit re-render).
      // Arm a bounded backstop: should the row never enter the render array
      // (e.g. a concurrent refetch dropped it), clear the stale pendingRef so a
      // later unrelated re-render can't fire a surprise jump.
      if (pendingRef.current) {
        if (settleTimerRef.current) clearTimeout(settleTimerRef.current);
        settleTimerRef.current = setTimeout(() => {
          settleTimerRef.current = null;
          const p = pendingRef.current;
          if (p && p.messageId === messageId) pendingRef.current = null;
        }, SETTLE_MS);
      }
    };

    // Fast path: target already loaded.
    const rows = getCachedRawMessages(activeSessionId, agent);
    if (rows.some((r) => r.id === messageId)) {
      finish(rows);
      return;
    }

    // Slow path: page older history until it appears (or we exhaust it).
    //
    // Deliberately bypasses the `loadPreviousMessages` store action — that
    // action reads `getLiveMessages(messageSource)`, which is `[]` in
    // `mode:"history"` (the mode the palette flow enters via selectSession),
    // so it early-returns without fetching and `hasMoreHistory` never
    // advances. Paging the React Query cache directly (same cache
    // `getCachedRawMessages` reads) works in every mode.
    let cancelled = false;
    const stale = () => cancelled || genRef.current !== gen;
    void (async () => {
      let current = rows;
      let fetchFailed = false;
      // I3: navigation.ts::selectSession invalidates this same query on every
      // selection; the refetch it triggers would land mid-backfill and replace
      // our prepended cache entry with a single fresh page, orphaning the
      // pending scroll. Cancel any in-flight/queued refetch before starting and
      // again after each prepend so the backfilled rows survive to resolution.
      void queryClient.cancelQueries({ queryKey: qk.sessionMessages(activeSessionId) });
      for (let i = 0; i < MAX_BACKFILL_PAGES; i++) {
        if (stale()) return;
        const beforeId = current[0]?.id;
        if (!beforeId) break; // nothing cached to anchor an older-page fetch on

        let page: MessageRow[];
        try {
          const params = new URLSearchParams({
            before_id: beforeId,
            limit: String(BACKFILL_PAGE_SIZE),
            agent,
          });
          const res = await apiGet<MessagesResponse>(
            `/api/sessions/${activeSessionId}/messages?${params.toString()}`,
          );
          page = res.messages ?? [];
        } catch {
          fetchFailed = true; // transient failure — distinct from genuine exhaustion
          break;
        }
        if (stale()) return;
        void queryClient.cancelQueries({ queryKey: qk.sessionMessages(activeSessionId) });

        if (page.length > 0) {
          prependOlderRawMessages(activeSessionId, agent, page);
          current = [...page, ...current];
          if (current.some((r) => r.id === messageId)) {
            finish(current);
            return;
          }
        }
        // A short page (fewer rows than requested) means history is exhausted.
        if (page.length < BACKFILL_PAGE_SIZE) break;
      }
      if (stale()) return;
      settled = true;
      // A fetch failure is a transient/generic error, not proof the message is
      // too far back — surface the generic error copy; `too_deep` stays for the
      // genuine exhausted-history case (a short page / no anchor).
      if (!silent) toast.error(t(fetchFailed ? "palette.open_error" : "palette.too_deep"));
      setTarget(null);
      pendingRef.current = null;
    })();

    return () => {
      cancelled = true;
      // Only an UNSETTLED resolution (superseded by a fresh target, or torn
      // down because the session/agent changed) leaves a stray pendingRef for
      // the `messages`-effect below to wrongly act on later — clear it in that
      // case only. A settled resolution (finish()/exhaustion, both of which
      // already decided pendingRef's final value themselves) must not have its
      // deliberately-still-pending target erased by its own setTarget(null).
      if (!settled) pendingRef.current = null;
    };
    // messagesRef/tryScroll are refs/stable; `messages` is intentionally NOT a
    // dep — the scroll effect below owns the post-re-render scroll.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target, activeSessionId, agent, setTarget, t, historyReady]);

  // ── Scroll once the resolved target enters the rendered window ──────────────
  useEffect(() => {
    tryScroll();
  }, [messages, tryScroll]);

  // Clean up dangling timers on unmount.
  useEffect(
    () => () => {
      if (highlightTimerRef.current) clearTimeout(highlightTimerRef.current);
      if (settleTimerRef.current) clearTimeout(settleTimerRef.current);
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
