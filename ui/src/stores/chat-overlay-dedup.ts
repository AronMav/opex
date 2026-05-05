/**
 * Chat live-overlay dedup.
 *
 * History is React Query truth. Live is the SSE buffer (optimistic user
 * message + the in-flight assistant message accumulating across all LLM
 * tool-loop iterations).
 *
 * Architecture (post-step-boundary refactor):
 *  • Backend emits one MessageStart per assistant turn and one StepStart per
 *    LLM iteration within that turn.
 *  • Frontend stream-processor inserts a StepBoundaryPart at every step-start
 *    so each iteration's text + tools live in their own structural slice.
 *    Repeated narration is no longer a confusing duplicate — it's a labeled
 *    "next step" event with a visible divider.
 *  • This dedup function therefore only handles ID-based dedup and the
 *    history/live continuation case. No content-based or intermediate-text
 *    heuristics.
 *
 * Rules:
 *  1. ID-based dedup: skip a live message whose id is already in history
 *     (finalize wrote the DB row with the same pre-allocated UUID).
 *  2. Tool dedup: drop tool parts whose toolCallId is already represented
 *     in history (parallel tool results may arrive in different order than
 *     they were declared).
 *  3. Continuation merge: when no new user message in live and history doesn't
 *     end with a user message, the live parts are appended to the last history
 *     assistant — same as convertHistory does for intermediate iterations.
 *  4. Otherwise: a new live assistant message is pushed (or merged into the
 *     previous live overlay assistant when no user message separates them).
 */

import type { ChatMessage, MessagePart, ReasoningPart, TextPart, ToolPart } from "./chat-types";

const MIN_DEDUP_TEXT_LEN = 20;

/**
 * Dedup safety net: within a single step (between two step-boundary parts),
 * drop text/reasoning parts whose trimmed content has already appeared.
 * Crosses step-boundary → set resets, so duplicates ACROSS iterations are
 * preserved as semantically valid "same narration on next step".
 *
 * Defends against duplicate SSE events that may arrive on reconnect replay,
 * stream-registry edge cases, or any path that pushes the same text twice
 * within one iteration. Backend should not produce these but UI must be
 * robust to them.
 */
function dedupeWithinSteps(parts: MessagePart[]): MessagePart[] {
  if (parts.length < 2) return parts;
  let seen = new Set<string>();
  const result: MessagePart[] = [];
  let dropped = false;
  for (const p of parts) {
    if (p.type === "step-boundary") {
      seen = new Set();
      result.push(p);
      continue;
    }
    if (p.type === "text" || p.type === "reasoning") {
      const t = (p as TextPart | ReasoningPart).text.trim();
      if (t.length >= MIN_DEDUP_TEXT_LEN && seen.has(t)) {
        dropped = true;
        continue;
      }
      if (t) seen.add(t);
    }
    result.push(p);
  }
  return dropped ? result : parts;
}

export function mergeLiveOverlay(
  historyMessages: ChatMessage[],
  liveMessages: ChatMessage[],
): ChatMessage[] {
  if (liveMessages.length === 0) return historyMessages;

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

  // True when history ends with a user message after the last assistant.
  // In that case live assistants are a NEW response, not a continuation of
  // the previous assistant turn — continuation merge must not fire.
  const historyEndsWithNewUserTurn =
    lastHistAssistantIdx >= 0 &&
    historyMessages.slice(lastHistAssistantIdx + 1).some((m) => m.role === "user");

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
      if (historyIds.has(m.id)) continue;

      const parts = m.parts.filter(
        (p) => p.type !== "tool" || !historyToolIds.has((p as ToolPart).toolCallId),
      );
      if (parts.length === 0) continue;

      // Continuation merge into the last history assistant when same user turn.
      if (!liveHasNewUserMsg && !historyEndsWithNewUserTurn && lastHistAssistantIdx >= 0) {
        continuationParts.push(...parts);
        continue;
      }

      // Merge with the previous live overlay assistant — collapses live
      // ChatMessages of the current turn into one bubble. Step boundaries
      // already separate iterations within the merged parts list.
      const lastOverlay = overlay.length > 0 ? overlay[overlay.length - 1] : null;
      if (lastOverlay?.role === "assistant") {
        overlay[overlay.length - 1] = { ...lastOverlay, parts: [...lastOverlay.parts, ...parts] };
      } else {
        overlay.push({ ...m, parts });
      }
    }
  }

  // Apply within-step dedup as a final safety net. Pure post-processor:
  // does not change merge rules, only collapses identical text parts that
  // landed inside the same step boundary (e.g. from reconnect replay).
  const dedupeAssistant = (m: ChatMessage): ChatMessage => {
    if (m.role !== "assistant") return m;
    const deduped = dedupeWithinSteps(m.parts);
    return deduped === m.parts ? m : { ...m, parts: deduped };
  };

  if (continuationParts.length > 0 && lastHistAssistantIdx >= 0) {
    const updated = [...historyMessages];
    const last = updated[lastHistAssistantIdx];
    const merged = { ...last, parts: [...last.parts, ...continuationParts] };
    updated[lastHistAssistantIdx] = dedupeAssistant(merged);
    const tailOverlay = overlay.map(dedupeAssistant);
    return tailOverlay.length > 0 ? [...updated, ...tailOverlay] : updated;
  }

  if (overlay.length === 0) return historyMessages;
  return [...historyMessages, ...overlay.map(dedupeAssistant)];
}
