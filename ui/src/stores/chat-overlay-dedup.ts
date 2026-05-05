/**
 * Chat live-overlay dedup (Architecture C).
 *
 * History is React Query truth. Live is the SSE buffer (optimistic user
 * message + in-flight assistant). Rules:
 *
 * User messages: ID-based. Client pre-allocates UUID in sendMessage(), sends as
 * user_message_id; bootstrap saves with that UUID via save_message_ex_with_id.
 *
 * Assistant messages:
 *   - ID-based dedup: if the live assistant ID is already in history (stream
 *     complete and finalize wrote the row), skip it.
 *   - Continuation merge: if the live assistant is NOT yet in history and
 *     there is no new user message in the overlay (= same turn), merge its
 *     unique parts (dedup by toolCallId) into the last history assistant.
 *     This mirrors what convertHistory does for completed iterations inside
 *     a tool-call loop, so the view is consistent in both live and history mode.
 *
 * Empty assistant placeholders (parts.length === 0) are always filtered.
 */

import type { ChatMessage, MessagePart, ToolPart } from "./chat-types";

export function mergeLiveOverlay(
  historyMessages: ChatMessage[],
  liveMessages: ChatMessage[],
): ChatMessage[] {
  if (liveMessages.length === 0) return historyMessages;

  // Index history for O(1) lookups.
  const historyIds = new Set(historyMessages.map((m) => m.id));

  const historyToolIds = new Set<string>();
  let lastHistAssistantIdx = -1;
  for (let i = 0; i < historyMessages.length; i++) {
    const m = historyMessages[i];
    if (m.role === "assistant") {
      lastHistAssistantIdx = i;
      for (const p of m.parts) {
        if (p.type === "tool") historyToolIds.add((p as ToolPart).toolCallId);
      }
    }
  }

  let liveHasNewUserMsg = false;
  const continuationParts: MessagePart[] = [];
  const overlay: ChatMessage[] = [];

  for (const m of liveMessages) {
    if (m.parts.length === 0) continue;

    if (m.role === "user") {
      if (!historyIds.has(m.id)) {
        overlay.push(m);
        liveHasNewUserMsg = true;
      }
      continue;
    }

    if (m.role === "assistant") {
      // Already in history (finalize wrote the row with the same pre-allocated UUID).
      if (historyIds.has(m.id)) continue;

      // Dedup tool parts by toolCallId — parallel calls from the same iteration
      // may arrive in a different order than history already recorded them.
      const uniqueParts = m.parts.filter(
        (p) => p.type !== "tool" || !historyToolIds.has((p as ToolPart).toolCallId),
      );
      if (uniqueParts.length === 0) continue;

      // Continuation merge: no new user message → still the same user turn.
      // Merge into the last history assistant bubble (completing a previous turn),
      // or merge consecutive live iterations together (active tool-call loop).
      if (!liveHasNewUserMsg && lastHistAssistantIdx >= 0) {
        continuationParts.push(...uniqueParts);
        continue;
      }

      // Merge with the previous live overlay assistant when no user message
      // separates them — collapses all tool-loop iterations into one bubble.
      const lastOverlay = overlay.length > 0 ? overlay[overlay.length - 1] : null;
      if (lastOverlay?.role === "assistant") {
        overlay[overlay.length - 1] = { ...lastOverlay, parts: [...lastOverlay.parts, ...uniqueParts] };
      } else {
        overlay.push({ ...m, parts: uniqueParts });
      }
    }
  }

  // Attach continuation parts to the last history assistant bubble.
  if (continuationParts.length > 0 && lastHistAssistantIdx >= 0) {
    const updated = [...historyMessages];
    const last = updated[lastHistAssistantIdx];
    updated[lastHistAssistantIdx] = { ...last, parts: [...last.parts, ...continuationParts] };
    return overlay.length > 0 ? [...updated, ...overlay] : updated;
  }

  return overlay.length > 0 ? [...historyMessages, ...overlay] : historyMessages;
}
