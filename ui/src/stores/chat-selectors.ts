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
import { getLiveMessages, isActivePhase } from "./chat-types";

// Re-export so consumers don't need a second import site.
export { useShallow };

// ── State shape alias ────────────────────────────────────────────────────────

export type ChatStoreState = ChatStore;

// ── Agent-scoped helpers ─────────────────────────────────────────────────────

/** Currently-selected agent name. Stable primitive — no `useShallow` required. */
export const selectCurrentAgent = (s: ChatStoreState): string => s.currentAgent;

/**
 * Per-agent state object. The store writes the `agents[agent]` slot immutably,
 * so the object identity is stable between unrelated updates — safe for direct
 * subscription, but callers that only read a subset SHOULD use the narrower
 * selectors below to avoid unnecessary re-renders.
 */
/** Active session id for the CURRENT agent. */
export const selectCurrentActiveSessionId = (s: ChatStoreState): string | null =>
  s.agents[s.currentAgent]?.activeSessionId ?? null;

/**
 * True when the CURRENT agent's turn is in an active phase (submitted/streaming).
 * Fix L: BranchNavigator uses this to disable branch switching mid-turn — a
 * branch switch during a live turn would blend two branches' lineages.
 */
export const selectCurrentPhaseIsActive = (s: ChatStoreState): boolean =>
  isActivePhase(s.agents[s.currentAgent]?.connectionPhase);

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
 * Id-keyed merge of the branch-resolved history with the live-turn overlay.
 *
 * The render is keyed on message id, NOT on positions/boundaries:
 *  - For a shared id present in BOTH history and live, the LIVE message wins —
 *    in-flight content (a streaming assistant, or a fuller resumed partial) is
 *    always fresher than any persisted history row under the same id. This is
 *    what makes the optimistic user echo and the persisted user row (same id)
 *    render exactly once regardless of whether history has refetched yet.
 *  - Live-only messages (not yet persisted to history) append after history in
 *    their original order.
 *
 * This replaces the previous positional boundary-slice model
 * (`concatLiveOntoHistory(historyUpToIncluding(history, boundary), live)`),
 * which double-rendered the turn's user message once history refetched to
 * include it (the boundary id WAS that user id, and the slice was inclusive).
 */
export function mergeRender(history: ChatMessage[], live: ChatMessage[]): ChatMessage[] {
  const liveById = new Map(live.map((m) => [m.id, m]));
  const seen = new Set<string>();
  const out: ChatMessage[] = [];
  for (const h of history) {
    out.push(liveById.get(h.id) ?? h);
    seen.add(h.id);
  }
  for (const m of live) {
    if (!seen.has(m.id)) {
      seen.add(m.id);
      out.push(m);
    }
  }
  return out;
}

/**
 * Returns the array to render for the given agent. Resolves the
 * three-way `messageSource` tag into a single array:
 *  - "new-chat"  → []
 *  - "history"   → cached history messages, resolved against selectedBranches
 *  - "live" / "finishing" → id-keyed merge of the FULL branch-resolved history
 *    with the live turn overlay (see `mergeRender`; live wins for shared ids,
 *    live-only messages append). No positional boundary slice.
 *
 * Reads `messageSource`, `selectedBranches`, `activeSessionId`, and (for
 * history mode) the React Query cache via getCachedHistoryMessages from the
 * passed state — no separate hook subscription needed in consumers.
 */
export function selectRenderMessages(state: ChatState, agent: string): ChatMessage[] {
  const st = state.agents[agent];
  if (!st) return [];
  const src = st.messageSource;
  if (src.mode === "new-chat") return [];
  if (src.mode === "history") {
    return getCachedHistoryMessages(src.sessionId, agent, st.selectedBranches);
  }
  if (src.mode === "finishing") {
    // Frozen live turn stays visible while React Query refetches history.
    return mergeRender(getCachedHistoryMessages(src.sessionId, agent, st.selectedBranches), src.messages);
  }
  // live mode
  const histSessionId = st.activeSessionId;
  const history = histSessionId ? getCachedHistoryMessages(histSessionId, agent, st.selectedBranches) : [];
  return mergeRender(history, src.messages);
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
