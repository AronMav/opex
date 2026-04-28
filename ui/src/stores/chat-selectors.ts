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
import { getLiveMessages } from "./chat-types";
import { getCachedHistoryMessages } from "./chat-history";
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

/**
 * Resolve the currently-visible message list for an agent. Delegates to the
 * appropriate source: live stream buffer or React-Query-cached history rows.
 * NOTE: returns a fresh array — callers that just want a single message should
 * prefer the single-message selector below, which is O(n) lookup but returns
 * the stable object reference stored in the buffer.
 */
export const selectVisibleMessages = (agent: string) =>
  (s: ChatStoreState): ChatMessage[] => {
    const st = s.agents[agent];
    if (!st) return [];
    if (st.messageSource.mode === "live") return getLiveMessages(st.messageSource);
    if (st.messageSource.mode === "history") {
      return getCachedHistoryMessages(st.activeSessionId, st.selectedBranches);
    }
    return [];
  };

/**
 * Find a message by id in the currently-visible message list for an agent.
 *
 * Returns the stable object reference stored in the buffer — two calls with
 * the same underlying message return the same reference, so `React.memo`
 * default shallow prop comparison will NOT trigger a re-render unless the
 * message actually changed.
 */
export const selectMessageById = (agent: string, messageId: string) =>
  (s: ChatStoreState): ChatMessage | undefined => {
    const st = s.agents[agent];
    if (!st) return undefined;
    if (st.messageSource.mode === "live") {
      return st.messageSource.messages.find((m) => m.id === messageId);
    }
    if (st.messageSource.mode === "history") {
      const msgs = getCachedHistoryMessages(st.activeSessionId, st.selectedBranches);
      return msgs.find((m) => m.id === messageId);
    }
    return undefined;
  };

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
  // live mode
  const histSessionId = st.activeSessionId;
  const history = histSessionId ? getCachedHistoryMessages(histSessionId, st.selectedBranches) : [];
  return mergeLiveOverlay(history, src.messages);
}
