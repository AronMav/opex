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
 * Dedup safety net for an assistant bubble:
 *  • within a single step (between step-boundaries), drop text/reasoning
 *    whose trimmed content has already appeared (≥20 chars only — short
 *    legitimate utterances stay)
 *  • collapse consecutive step-boundary markers (extras come from
 *    convertHistory + live overlay both inserting one at the same point)
 *  • drop a trailing step-boundary that has no content after it
 *
 * Crossing step-boundary resets the per-step set so duplicates ACROSS
 * iterations remain (semantically valid: same narration on next step).
 */
function dedupeWithinSteps(parts: MessagePart[]): MessagePart[] {
  if (parts.length < 2) return parts;
  let seen = new Set<string>();
  const result: MessagePart[] = [];
  let dropped = false;
  for (const p of parts) {
    if (p.type === "step-boundary") {
      // Skip when previous emitted part is also a boundary.
      const prev = result[result.length - 1];
      if (prev && prev.type === "step-boundary") {
        dropped = true;
        continue;
      }
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
  // Drop trailing boundary (no content after it).
  while (result.length > 0 && result[result.length - 1].type === "step-boundary") {
    result.pop();
    dropped = true;
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
  // Text content already shown in the LAST history assistant — needed because
  // SSE replay (e.g. backend StreamRegistry on reconnect) re-emits the same
  // text events that convertHistory already turned into parts. Without
  // content-based dedup the continuation merge duplicates them in the bubble.
  let lastHistAssistantTexts = new Set<string>();
  let lastHistAssistantIdx = -1;
  for (let i = 0; i < historyMessages.length; i++) {
    const m = historyMessages[i];
    if (m.role === "assistant") {
      lastHistAssistantIdx = i;
      for (const p of m.parts) {
        if (p.type === "tool") historyToolIds.add((p as ToolPart).toolCallId);
      }
      // Refresh per-assistant — only the LAST one's texts go into the set.
      lastHistAssistantTexts = new Set();
      for (const p of m.parts) {
        if (p.type === "text") {
          const t = (p as TextPart).text.trim();
          if (t) lastHistAssistantTexts.add(t);
        }
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
        // Filter parts whose content is already represented in the target
        // bubble (text by trimmed content, tools by id). SSE replay on
        // reconnect re-emits everything — without this filter the live
        // overlay duplicates everything convertHistory already produced.
        const dedupedParts = parts.filter((p) => {
          if (p.type === "text") {
            const t = (p as TextPart).text.trim();
            return !t || !lastHistAssistantTexts.has(t);
          }
          return true; // tools already filtered by historyToolIds above
        });
        if (dedupedParts.length > 0) continuationParts.push(...dedupedParts);
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
