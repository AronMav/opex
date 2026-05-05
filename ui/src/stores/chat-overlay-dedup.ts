/**
 * Chat live-overlay dedup (Architecture C).
 *
 * History is React Query truth. Live is the SSE buffer. Each LLM tool-loop
 * iteration emits its own `start` event and creates a separate ChatMessage
 * in the live buffer with a fresh UUID.
 *
 * Rules:
 *   1. ID-based dedup: skip a live message whose id is already in history
 *      (finalize wrote the DB row with the same pre-allocated UUID).
 *   2. Intermediate-iteration text suppression: when multiple non-history
 *      assistant messages exist in live (= an active tool-call loop), the
 *      EARLIER iterations show only their tool/file/etc. parts; their text
 *      and reasoning are dropped. Only the LAST live assistant — the one
 *      currently streaming — shows its text. This avoids the visual
 *      "Делегирую..." → tool → "Делегирую..." → tool → "Делегирую..." stutter
 *      caused by models that repeat the same intro narration on every iteration.
 *   3. Continuation merge: when no new user message in live and history doesn't
 *      end with a user message, the live parts are appended to the last history
 *      assistant — same as convertHistory does for completed intermediate iterations.
 *   4. Otherwise: a new live assistant message is pushed (or merged into the
 *      previous live overlay assistant when no user message separates them).
 */

import type { ChatMessage, MessagePart, ReasoningPart, TextPart, ToolPart } from "./chat-types";

// Below this length a duplicated text/reasoning is more likely a legitimate
// short utterance ("OK", "yes") than an LLM-repeating-its-intro artifact.
const MIN_DEDUP_TEXT_LEN = 20;

/**
 * Removes text/reasoning parts whose trimmed content has already appeared
 * in the same bubble. The LLM-tool-loop architecture emits one DB row per
 * iteration, and many models repeat the same intro narration on every
 * iteration — without this pass, a single rendered bubble shows the same
 * text two or three times.
 *
 * The proper architectural fix is per-iteration step-start boundaries
 * (Vercel AI SDK pattern); this is a content-level safety net.
 */
export function dedupeBubbleTextParts(parts: MessagePart[]): MessagePart[] {
  if (parts.length < 2) return parts;
  const seen = new Set<string>();
  const result: MessagePart[] = [];
  let dropped = false;
  for (const p of parts) {
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

  // Find the index of the LAST non-history live assistant with content.
  // That iteration is the "current" one whose narration we keep; earlier
  // iterations are intermediate and contribute only their action parts.
  let lastLiveAssistantIdx = -1;
  for (let i = liveMessages.length - 1; i >= 0; i--) {
    const m = liveMessages[i];
    if (m.role === "assistant" && m.parts.length > 0 && !historyIds.has(m.id)) {
      lastLiveAssistantIdx = i;
      break;
    }
  }

  let liveHasNewUserMsg = false;
  const continuationParts: MessagePart[] = [];
  const overlay: ChatMessage[] = [];

  for (let i = 0; i < liveMessages.length; i++) {
    const m = liveMessages[i];
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

      let parts = m.parts.filter(
        (p) => p.type !== "tool" || !historyToolIds.has((p as ToolPart).toolCallId),
      );

      // Intermediate iterations: drop text/reasoning. Their narration is
      // typically a near-duplicate of the next iteration's narration; keep
      // only the actions (tool, file, rich-card, approval, step-group).
      const isCurrentIteration = i === lastLiveAssistantIdx;
      if (!isCurrentIteration) {
        parts = parts.filter((p) => p.type !== "text" && p.type !== "reasoning");
      }

      if (parts.length === 0) continue;

      // Continuation merge into the last history assistant when same user turn.
      if (!liveHasNewUserMsg && !historyEndsWithNewUserTurn && lastHistAssistantIdx >= 0) {
        continuationParts.push(...parts);
        continue;
      }

      // Merge with the previous live overlay assistant — collapses all
      // tool-loop iterations of the current turn into one bubble.
      const lastOverlay = overlay.length > 0 ? overlay[overlay.length - 1] : null;
      if (lastOverlay?.role === "assistant") {
        overlay[overlay.length - 1] = { ...lastOverlay, parts: [...lastOverlay.parts, ...parts] };
      } else {
        overlay.push({ ...m, parts });
      }
    }
  }

  // Apply per-bubble text dedup as the last safety net. Pure post-processor:
  // doesn't change rules above, only collapses any text duplicates that slipped
  // through (e.g. live continuation merging text already present in history).
  const dedupeAssistant = (m: ChatMessage): ChatMessage => {
    if (m.role !== "assistant") return m;
    const deduped = dedupeBubbleTextParts(m.parts);
    return deduped === m.parts ? m : { ...m, parts: deduped };
  };

  if (continuationParts.length > 0 && lastHistAssistantIdx >= 0) {
    const updated = [...historyMessages];
    const last = updated[lastHistAssistantIdx];
    const mergedParts = [...last.parts, ...continuationParts];
    const dedupedParts = dedupeBubbleTextParts(mergedParts);
    updated[lastHistAssistantIdx] = { ...last, parts: dedupedParts };
    const tailOverlay = overlay.map(dedupeAssistant);
    return tailOverlay.length > 0 ? [...updated, ...tailOverlay] : updated;
  }

  if (overlay.length === 0) return historyMessages;
  return [...historyMessages, ...overlay.map(dedupeAssistant)];
}
