/**
 * Chat live-overlay dedup (Architecture C, simplified).
 *
 * History is React Query truth. Live is the SSE buffer (optimistic user
 * message + in-flight assistant). Merge rule: append live messages whose
 * ID is not yet in history, filtering empty assistant placeholders.
 *
 * ID-based dedup works because the backend now pre-allocates the assistant
 * message UUID and sends it in the `start` SSE event. The live buffer uses
 * the same ID as the eventual DB row, so `historyIds.has(m.id)` correctly
 * detects when history has caught up.
 */

import type { ChatMessage } from "./chat-types";

export function mergeLiveOverlay(
  historyMessages: ChatMessage[],
  liveMessages: ChatMessage[],
): ChatMessage[] {
  if (liveMessages.length === 0) return historyMessages;

  const historyIds = new Set(historyMessages.map((m) => m.id));
  const extra = liveMessages.filter(
    (m) => !historyIds.has(m.id) && m.parts.length > 0,
  );

  return extra.length > 0 ? [...historyMessages, ...extra] : historyMessages;
}
