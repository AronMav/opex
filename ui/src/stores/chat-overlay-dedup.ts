/**
 * Chat live-overlay dedup (Architecture C).
 *
 * History is React Query truth. Live is the SSE buffer (optimistic user
 * message + in-flight assistant). Merge rule: append live messages whose
 * ID is not yet in history, filtering empty assistant placeholders.
 *
 * ID-based dedup works for both roles because:
 * - Assistant: backend pre-allocates UUID in execute.rs, sends it in the
 *   `start` SSE event (Task 1). Live buffer uses the same ID as the DB row.
 * - User: client pre-allocates UUID in sendMessage(), sends it as
 *   `user_message_id` in the request body. Bootstrap uses it via
 *   `save_message_ex_with_id` (same pattern as assistant).
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
