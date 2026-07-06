/**
 * Chat live-overlay dedup — pure ID-based after the per-iteration UUID rework.
 *
 * History is React Query truth: each DB row → one ChatMessage with row.id.
 * Live is the SSE buffer: each tool-loop iteration starts on `step-start`,
 * which carries the pre-allocated UUID of the row this iteration WILL produce.
 * The frontend opens a fresh live ChatMessage with that exact UUID.
 *
 * Therefore live.id === history.id for every saved iteration, and dedup
 * collapses to a single check: skip a live ChatMessage whose id is already
 * present in history. No content matching, no per-step boundaries, no
 * continuation merge — those were all compensations for the ID gap that
 * Phase 1 of the rework eliminated.
 *
 * Tool-id dedup remains (parallel tool results may arrive in any order, and
 * a row already in history has its tools attached as ChatMessage parts).
 *
 * Multiple in-flight live iterations (the next iteration starts before its
 * predecessor's row is persisted) merge into one visual bubble so the user
 * sees a coherent assistant turn, not a stack of half-empty bubbles.
 */

import type { ChatMessage, ToolPart } from "./chat-types";

interface HistoryIndex {
  ids: Set<string>;
  toolIds: Set<string>;
}

/**
 * WeakMap cache of pre-computed history indexes keyed by the ChatMessage[]
 * reference. PERF-02 in-place Immer mutations keep history arrays stable
 * across renders that don't change DB content, so this cache hits the
 * common case (typing into composer, focusing window, scrolling) where
 * mergeLiveOverlay is called many times with the same history.
 *
 * GC-safe: entries vanish when the array goes out of scope.
 */
const historyIndexCache = new WeakMap<readonly ChatMessage[], HistoryIndex>();

function buildHistoryIndex(history: readonly ChatMessage[]): HistoryIndex {
  const cached = historyIndexCache.get(history);
  if (cached) return cached;
  const ids = new Set<string>();
  const toolIds = new Set<string>();
  for (const m of history) {
    ids.add(m.id);
    if (m.mergedIds) for (const mid of m.mergedIds) ids.add(mid);
    if (m.role === "assistant") {
      for (const p of m.parts) {
        if (p.type === "tool") toolIds.add((p as ToolPart).toolCallId);
      }
    }
  }
  const idx: HistoryIndex = { ids, toolIds };
  historyIndexCache.set(history, idx);
  return idx;
}

export function mergeLiveOverlay(
  historyMessages: ChatMessage[],
  liveMessages: ChatMessage[],
): ChatMessage[] {
  if (liveMessages.length === 0) return historyMessages;

  // historyIds includes both the primary id of each ChatMessage AND any
  // mergedIds — convertHistory merges multiple intermediate DB rows into
  // one bubble keyed by the first row's id, but every merged row is a
  // valid match target for live dedup.
  const { ids: historyIds, toolIds: historyToolIds } = buildHistoryIndex(historyMessages);

  const overlay: ChatMessage[] = [];

  for (const m of liveMessages) {
    if (m.parts.length === 0) continue;
    if (historyIds.has(m.id)) continue;

    if (m.role === "user") {
      overlay.push(m);
      continue;
    }

    if (m.role === "assistant") {
      const parts = m.parts.filter(
        (p) => p.type !== "tool" || !historyToolIds.has((p as ToolPart).toolCallId),
      );
      if (parts.length === 0) continue;

      // Merge consecutive in-flight live iterations into one visual bubble.
      // The previous overlay item is "consecutive" only when it's an assistant
      // (no user message between us and it). Once an iteration's row is
      // persisted it disappears from the overlay (history wins by id), so
      // this merge only collapses iterations that haven't been saved yet.
      const lastOverlay = overlay.length > 0 ? overlay[overlay.length - 1] : null;
      if (lastOverlay?.role === "assistant") {
        overlay[overlay.length - 1] = { ...lastOverlay, parts: [...lastOverlay.parts, ...parts] };
      } else {
        overlay.push({ ...m, parts });
      }
    }
  }

  if (overlay.length === 0) return historyMessages;
  return [...historyMessages, ...overlay];
}
