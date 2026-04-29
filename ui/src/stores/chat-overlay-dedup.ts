/**
 * Chat live-overlay dedup helper (Architecture C).
 *
 * History comes from React Query (DB truth). Live comes from SSE stream +
 * optimistic user bubble. To avoid flashing duplicates as history catches up,
 * we merge live on top of history with these rules:
 *
 *  - User messages: keep the optimistic/live copy until a DB row with the
 *    same first-text appears in history. Status ("sending" / "confirmed" /
 *    "failed") is IRRELEVANT — the only thing that matters is "is this
 *    message mirrored in history yet?". Prior logic gated on status === "sending"
 *    and caused the user bubble to disappear the instant the server acknowledged
 *    the send (data-session-id event) but before history refetched.
 *  - Assistant messages: dedup tool parts by toolCallId only. Text parts are
 *    NOT deduped by content — false positives when the model repeats a phrase
 *    across iterations (e.g. "Цикл 2 — взаимная критика. Отправляю ...") would
 *    swallow the start of the new message until the live text diverged from
 *    the previous saved one. Whole-message dedup by ID below covers history
 *    catching up.
 *  - Empty assistant placeholders are filtered — ThinkingMessage handles them.
 */

import type { ChatMessage } from "./chat-types";

export function mergeLiveOverlay(
  historyMessages: ChatMessage[],
  liveMessages: ChatMessage[],
): ChatMessage[] {
  if (liveMessages.length === 0) return historyMessages;

  // Index history for O(1) lookups during overlay walk.
  const historyIds = new Set(historyMessages.map(m => m.id));
  const historyToolIds = new Set<string>();
  const historyUserTexts = new Set<string>();
  for (const m of historyMessages) {
    if (m.role === "assistant") {
      for (const p of m.parts) {
        if (p.type === "tool") historyToolIds.add(p.toolCallId);
      }
    } else if (m.role === "user") {
      const first = m.parts?.[0];
      if (first?.type === "text" && first.text) historyUserTexts.add(first.text);
    }
  }

  const overlay: ChatMessage[] = [];
  for (const m of liveMessages) {
    if (m.role === "assistant" && m.parts.length === 0) continue;
    // Skip assistant messages already present in history — prevents stale live
    // overlays (reconnect replays, post-finish flicker) from creating duplicates.
    if (m.role === "assistant" && historyIds.has(m.id)) continue;

    if (m.role === "user") {
      const firstText = m.parts?.[0]?.type === "text" ? (m.parts[0] as { text: string }).text : "";
      if (!firstText) {
        // H5: empty / attachment-only messages all map to "" — fall back to ID dedup
        if (!historyIds.has(m.id)) overlay.push(m);
      } else if (!historyUserTexts.has(firstText)) {
        overlay.push(m);
      }
      continue;
    }

    if (m.role === "assistant") {
      const uniqueParts = m.parts.filter((p) => {
        if (p.type === "tool") return !historyToolIds.has(p.toolCallId);
        return true;
      });
      if (uniqueParts.length === 0) continue;
      overlay.push({ ...m, parts: uniqueParts });
      continue;
    }

    overlay.push(m);
  }

  return overlay.length > 0 ? [...historyMessages, ...overlay] : historyMessages;
}
