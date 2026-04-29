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

import type { ChatMessage, MessagePart } from "./chat-types";

export function mergeLiveOverlay(
  historyMessages: ChatMessage[],
  liveMessages: ChatMessage[],
): ChatMessage[] {
  if (liveMessages.length === 0) return historyMessages;

  // Index history for O(1) lookups during overlay walk.
  const historyIds = new Set(historyMessages.map(m => m.id));
  const historyToolIds = new Set<string>();
  const historyUserTexts = new Set<string>();
  // Text parts of the last history assistant — used to dedup repeated preamble
  // text that the model emits at the start of every tool-call iteration.
  const lastHistAssistantTexts = new Set<string>();
  let lastHistAssistantIdx = -1;

  for (let i = 0; i < historyMessages.length; i++) {
    const m = historyMessages[i];
    if (m.role === "assistant") {
      for (const p of m.parts) {
        if (p.type === "tool") historyToolIds.add(p.toolCallId);
      }
      lastHistAssistantIdx = i;
      lastHistAssistantTexts.clear();
      for (const p of m.parts) {
        if (p.type === "text" && p.text?.trim()) lastHistAssistantTexts.add(p.text.trim());
      }
    } else if (m.role === "user") {
      const first = m.parts?.[0];
      if (first?.type === "text" && first.text) historyUserTexts.add(first.text);
    }
  }

  const lastHistMsg = lastHistAssistantIdx >= 0 ? historyMessages[lastHistAssistantIdx] : null;

  // Continuation parts to merge into the last history assistant instead of
  // creating a new bubble. Only used when no new user message appears in the
  // live overlay before the live assistant (same turn, not a new one).
  let liveHasNewUserMsg = false;
  const continuationParts: MessagePart[] = [];
  const overlay: ChatMessage[] = [];

  for (const m of liveMessages) {
    if (m.role === "assistant" && m.parts.length === 0) continue;
    // Skip assistant messages already present in history — prevents stale live
    // overlays (reconnect replays, post-finish flicker) from creating duplicates.
    if (m.role === "assistant" && historyIds.has(m.id)) continue;

    if (m.role === "user") {
      const firstText = m.parts?.[0]?.type === "text" ? (m.parts[0] as { text: string }).text : "";
      let isNew = false;
      if (!firstText) {
        // H5: empty / attachment-only messages all map to "" — fall back to ID dedup
        if (!historyIds.has(m.id)) { overlay.push(m); isNew = true; }
      } else if (!historyUserTexts.has(firstText)) {
        overlay.push(m);
        isNew = true;
      }
      // Only mark as "new user turn" when the user message itself is new (not yet
      // in history). If history already has it (confirmed), the assistant streaming
      // is a continuation of the same turn and continuation-merge must still apply.
      if (isNew) liveHasNewUserMsg = true;
      continue;
    }

    if (m.role === "assistant") {
      // Continuation check: is this live assistant a continuation of the last
      // history assistant (same named agent, same turn, no new user message)?
      // Requires both agentIds to be explicitly set — undefined/undefined would
      // be a false positive (different messages from different contexts).
      const isContinuation =
        !!lastHistMsg &&
        !liveHasNewUserMsg &&
        !!m.agentId &&
        !!lastHistMsg.agentId &&
        m.agentId === lastHistMsg.agentId;

      const uniqueParts = m.parts.filter((p) => {
        if (p.type === "tool") return !historyToolIds.has(p.toolCallId);
        // Dedup text parts already shown in the last history assistant bubble.
        // The model repeats the same preamble ("Собираю данные...") at the start
        // of every tool-call iteration; without dedup these pile up as duplicate
        // text in the live overlay.  Only applies to confirmed continuation turns.
        if (p.type === "text" && isContinuation) {
          const t = p.text?.trim() ?? "";
          if (t && lastHistAssistantTexts.has(t)) return false;
        }
        return true;
      });
      if (uniqueParts.length === 0) continue;

      // Merge continuation turns into the last history assistant instead of
      // appending a separate bubble.
      if (isContinuation) {
        continuationParts.push(...uniqueParts);
        continue;
      }

      overlay.push({ ...m, parts: uniqueParts });
      continue;
    }

    overlay.push(m);
  }

  // Attach continuation parts to the last history assistant message.
  if (continuationParts.length > 0 && lastHistMsg) {
    const updated = [...historyMessages];
    updated[lastHistAssistantIdx] = { ...lastHistMsg, parts: [...lastHistMsg.parts, ...continuationParts] };
    return overlay.length > 0 ? [...updated, ...overlay] : updated;
  }

  return overlay.length > 0 ? [...historyMessages, ...overlay] : historyMessages;
}
