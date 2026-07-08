/**
 * Chat store selectors (REF-05)
 *
 * Typed, memoisation-friendly selectors over the chat-store state.
 * Consumers call these together with `useShallow` so re-renders only occur
 * when the selected slice actually changes by shallow equality — NOT on every
 * unrelated store update (streaming tick, sidebar toggle, sibling message etc).
 *
 * Re-exports `useShallow` so components do not import from `zustand/react/shallow`
 * directly; keeps the dependency graph legible.
 */

import { useShallow } from "zustand/react/shallow";

import { useChatStore } from "./chat-store";
import type {
  ChatMessage,
  ChatState,
  ChatStore,
} from "./chat-types";
import { getCachedHistoryMessages } from "./chat-history";
import { getLiveMessages } from "./chat-types";
import { mergeLiveOverlay } from "./chat-overlay-dedup";

// Re-export so consumers don't need a second import site.
export { useShallow };

// ── State shape alias ────────────────────────────────────────────────────────

export type ChatStoreState = ChatStore;

// ── Agent-scoped helpers ─────────────────────────────────────────────────────

const EMPTY_SELECTED_BRANCHES: Readonly<Record<string, string>> = Object.freeze({});

/** Currently-selected agent name. Stable primitive — no `useShallow` required. */
export const selectCurrentAgent = (s: ChatStoreState): string => s.currentAgent;

/**
 * Per-agent state object. The store writes the `agents[agent]` slot immutably,
 * so the object identity is stable between unrelated updates — safe for direct
 * subscription, but callers that only read a subset SHOULD use the narrower
 * selectors below to avoid unnecessary re-renders.
 */
/** Active session id for a given agent (null if none). */
export const selectActiveSessionId = (agent: string) =>
  (s: ChatStoreState): string | null => s.agents[agent]?.activeSessionId ?? null;

/** Active session id for the CURRENT agent. */
export const selectCurrentActiveSessionId = (s: ChatStoreState): string | null =>
  s.agents[s.currentAgent]?.activeSessionId ?? null;


/** Branch selection map (stable reference when empty). */
export const selectSelectedBranches = (agent: string) =>
  (s: ChatStoreState): Record<string, string> =>
    s.agents[agent]?.selectedBranches ?? (EMPTY_SELECTED_BRANCHES as Record<string, string>);

// ── Message selectors ────────────────────────────────────────────────────────


// ── Action selectors (stable references via Zustand) ─────────────────────────

/**
 * Bundle of actions MessageItem (and friends) need. Each property is a
 * stable function reference inside Zustand, so the returned object is
 * shallow-equal across renders — consumers paired with `useShallow` skip
 * re-rendering unless a store action itself is reassigned (never in practice).
 */
export const selectActions = (s: ChatStoreState) => ({
  deleteMessage: s.deleteMessage,
  regenerate: s.regenerate,
  regenerateFrom: s.regenerateFrom,
  switchBranch: s.switchBranch,
  forkAndRegenerate: s.forkAndRegenerate,
  sendMessage: s.sendMessage,
  stopStream: s.stopStream,
  exportSession: s.exportSession,
});

export type ChatActions = ReturnType<typeof selectActions>;

// ── Convenience hooks (optional) ────────────────────────────────────────────
//
// These are thin wrappers around `useChatStore(useShallow(selector))` so call
// sites can read naturally (`const actions = useChatActions()`) while still
// getting useShallow-based re-render gating.

/** Read the bundle of chat actions with shallow-equal re-render gating. */
export function useChatActions(): ChatActions {
  return useChatStore(useShallow(selectActions));
}

/** Read selected branches for a given agent with shallow-equal gating. */
export function useSelectedBranches(agent: string): Record<string, string> {
  return useChatStore(useShallow(selectSelectedBranches(agent)));
}

// ── Derived mode selectors ────────────────────────────────────────────────────

/** True when the agent has no active session (new-chat mode). */
export function selectIsEmpty(state: ChatState, agent: string): boolean {
  return state.agents[agent]?.messageSource.mode === "new-chat";
}

/** True when the agent is viewing a history snapshot (not a live stream). */
export function selectIsReplayingHistory(state: ChatState, agent: string): boolean {
  return state.agents[agent]?.messageSource.mode === "history";
}

/** True when the agent has an active or recently-completed SSE stream. */
export function selectIsLive(state: ChatState, agent: string): boolean {
  return state.agents[agent]?.messageSource.mode === "live";
}

/** True when in live mode and there is at least one message in the buffer. */
export function selectLiveHasContent(state: ChatState, agent: string): boolean {
  const src = state.agents[agent]?.messageSource;
  return src?.mode === "live" && src.messages.length > 0;
}

/**
 * Returns the array to render for the given agent. Resolves the
 * three-way `messageSource` tag into a single array:
 *  - "new-chat"  → []
 *  - "history"   → cached history messages, resolved against selectedBranches
 *  - "live"      → history merged with live overlay (optimistic + in-flight)
 *
 * Reads `messageSource`, `selectedBranches`, and (for history mode)
 * the React Query cache via getCachedHistoryMessages from the passed
 * state — no separate hook subscription needed in consumers.
 */
export function selectRenderMessages(state: ChatState, agent: string): ChatMessage[] {
  const st = state.agents[agent];
  if (!st) return [];
  const src = st.messageSource;
  if (src.mode === "new-chat") return [];
  if (src.mode === "history") {
    return getCachedHistoryMessages(src.sessionId, st.selectedBranches);
  }
  if (src.mode === "finishing") {
    // Frozen live messages remain visible while React Query refetches history.
    // Merge with whatever history is already cached so existing messages are shown.
    const history = getCachedHistoryMessages(src.sessionId, st.selectedBranches);
    return mergeLiveOverlay(history, src.messages);
  }
  // live mode
  const histSessionId = st.activeSessionId;
  const history = histSessionId ? getCachedHistoryMessages(histSessionId, st.selectedBranches) : [];
  return mergeLiveOverlay(history, src.messages);
}

const EMPTY_LIVE_TEXT = { id: "", text: "" };

/**
 * The id + concatenated text of the last assistant message in the live overlay
 * (live / finishing modes), or a stable empty value otherwise. Used by the
 * streaming a11y announcer; O(last message + its text parts), so it is cheap to
 * subscribe to on every streaming tick (with useShallow).
 */
export function selectLiveAssistantText(state: ChatState, agent: string): { id: string; text: string } {
  const src = state.agents[agent]?.messageSource;
  const live = src ? getLiveMessages(src) : [];
  for (let i = live.length - 1; i >= 0; i--) {
    const m = live[i];
    if (m.role === "assistant") {
      const text = m.parts.flatMap((p) => (p.type === "text" ? [p.text] : [])).join("");
      return { id: m.id, text };
    }
  }
  return EMPTY_LIVE_TEXT;
}
