// ── chat/actions/navigation.ts ──────────────────────────────────────────────
// Navigation actions extracted from chat-store.ts.
// Receives dependencies via ActionDeps — same get/set closures the immer
// factory provides, plus queryClient and the streaming renderer.

import type { ActionDeps } from "../../chat-store";
import { emptyAgentState } from "../../chat-types";
import type { AgentState } from "../../chat-types";
import { qk } from "@/lib/queries";
import { saveLastSession, clearLastSessionId } from "../../chat-persistence";
import { isActivePhase } from "../../chat-types";
import { selectIsReplayingHistory } from "../../chat-selectors";

export function createNavigationActions(deps: ActionDeps) {
  const { get, set, queryClient, renderer } = deps;

  // ── Internal helpers (mirroring store-level ensure/update) ──────────────

  function ensure(agent: string): AgentState {
    const s = get().agents[agent];
    if (s) return s;
    const fresh = emptyAgentState();
    set((draft: any) => { draft.agents[agent] = fresh; });
    return fresh;
  }

  function update(agent: string, patch: Partial<AgentState>) {
    set((draft: any) => {
      if (!draft.agents[agent]) draft.agents[agent] = emptyAgentState();
      Object.assign(draft.agents[agent], patch);
    });
  }

  // ── Navigation actions ───────────────────────────────────────────────────

  return {
    setCurrentAgent: (name: string) => {
      const prev = get().currentAgent;
      if (prev === name) return;

      // Page-load initialization (prev is empty) — just set the agent,
      // DON'T wipe session state. The restore effect in page.tsx will handle it.
      if (!prev) {
        ensure(name);
        set({ currentAgent: name });
        queryClient.invalidateQueries({ queryKey: qk.sessions(name) });
        return;
      }

      // Check if current session is multi-agent and includes the new agent
      const prevState = get().agents[prev];
      const activeSessionId = prevState?.activeSessionId;

      if (activeSessionId) {
        const participants = get().sessionParticipants[activeSessionId];
        if (participants && participants.includes(name)) {
          ensure(name);
          // Multi-agent session reuse: the new agent inherits the same
          // sessionId. Invalidate so React Query refetches the fresh DB
          // state under the new agent's query context.
          queryClient.invalidateQueries({ queryKey: qk.sessionMessages(activeSessionId) });
          update(name, {
            activeSessionId,
            messageSource: prevState?.messageSource ?? { mode: "new-chat" },
            connectionPhase: prevState?.connectionPhase ?? "idle",
          });
          set({ currentAgent: name });
          saveLastSession(name, activeSessionId);
          return;
        }
      }

      // User-initiated agent switch to a DIFFERENT session (or no shared
      // session). Invalidate the previous agent's session so returning to
      // it later shows fresh data.
      if (activeSessionId) {
        queryClient.invalidateQueries({ queryKey: qk.sessionMessages(activeSessionId) });
      }
      // MEM-01: clean up all Maps for previous agent
      renderer.cleanupAgent(prev);
      update(prev, { connectionPhase: "idle" });
      ensure(name);
      update(name, {
        activeSessionId: null,
        messageSource: { mode: "new-chat" },
        streamError: null,
        connectionPhase: "idle",
        connectionError: null,
        forceNewSession: true,
      });
      set({ currentAgent: name });
      clearLastSessionId(name);
      saveLastSession(name);
      queryClient.invalidateQueries({ queryKey: qk.sessions(name) });
    },

    selectSession: async (sessionId: string, forAgent?: string) => {
      const agent = forAgent ?? get().currentAgent;
      ensure(agent);

      // If re-selecting the same session that's currently streaming, just switch to live view
      const currentState = get().agents[agent];
      if (currentState?.activeSessionId === sessionId && isActivePhase(currentState.connectionPhase)) {
        // Already in live mode — no change needed (messageSource should already be live)
        return;
      }

      // Invalidate React Query cache for BOTH the previous active session
      // (its DB state may have changed after the aborted stream wrote partial
      // assistant text) AND the incoming session. Without this, returning to
      // a previously-streaming session showed stale cached data — the user's
      // initial message could be missing if the cache was populated before
      // the backend saved it. Regression 2026-04-18.
      const previousSessionId = currentState?.activeSessionId;
      if (previousSessionId && previousSessionId !== sessionId) {
        queryClient.invalidateQueries({ queryKey: qk.sessionMessages(previousSessionId) });
      }
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });

      // Local-only abort: tear down the UI fetch so the new session can
      // render, but DO NOT POST /abort to the backend. A POST here would
      // cancel the departing session's engine task — if its provider is
      // slow to acknowledge the cancel, the cancel-grace window exceeds
      // 30 s and the session gets marked `'interrupted'` in DB. The user
      // only switched tabs; they did not explicitly Stop. The backend
      // stream finishes on its own (10-minute SSE safety net covers
      // worst-case abandonment) and the completed response is waiting
      // when the user returns.
      //
      // Fix #8: this also covers "user picks a different session in the
      // sidebar while the current one is streaming" — abortLocalOnly()
      // bumps the StreamSession generation, so any in-flight SSE events
      // for the previous session bail out at session.isCurrent checks
      // and won't leak writes into the newly-selected session's state.
      // ChatThread's auto-resume effect picks up live continuation iff
      // the target session is itself running.
      renderer.abortLocalOnly(agent);

      update(agent, {
        activeSessionId: sessionId,
        messageSource: { mode: "history", sessionId },
        forceNewSession: false,
        renderLimit: 100,
      });
      saveLastSession(agent, sessionId);
    },

    selectSessionById: (agent: string, sessionId: string) => {
      // Switch to the agent and select the session
      set({ currentAgent: agent });
      ensure(agent);
      // Abort any active stream for this agent
      const currentState = get().agents[agent];
      const previousSessionId = currentState?.activeSessionId;
      if (previousSessionId && previousSessionId !== sessionId) {
        queryClient.invalidateQueries({ queryKey: qk.sessionMessages(previousSessionId) });
      }
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
      // See selectSession above — navigation must not cancel the backend.
      renderer.abortLocalOnly(agent);
      update(agent, {
        activeSessionId: sessionId,
        messageSource: { mode: "history", sessionId },
        forceNewSession: false,
        connectionPhase: "idle",
      });
      saveLastSession(agent, sessionId);
    },

    newChat: () => {
      const agent = get().currentAgent;
      // Invalidate the departing session's React Query cache — the stream
      // we are detaching from may still write partial assistant text to
      // DB. Without this, returning to that session via the sidebar shows
      // stale data.
      const previousSessionId = get().agents[agent]?.activeSessionId;
      if (previousSessionId) {
        queryClient.invalidateQueries({ queryKey: qk.sessionMessages(previousSessionId) });
      }
      // Local-only abort: starting a new chat does not imply the user
      // wants to cancel the previous response — they may want to see it
      // completed when they come back. See selectSession for the full
      // rationale.
      renderer.abortLocalOnly(agent);
      update(agent, {
        activeSessionId: null,
        messageSource: { mode: "new-chat" },
        streamError: null,
        connectionPhase: "idle",
        connectionError: null,
        forceNewSession: true,
      });
      saveLastSession(agent);
    },

    markSessionActive: (agent: string, sessionId: string) => {
      ensure(agent);
      set((draft: any) => {
        const st = draft.agents[agent];
        if (!st) return;
        if (!st.activeSessionIds.includes(sessionId)) {
          st.activeSessionIds.push(sessionId);
        }
      });
    },

    markSessionInactive: (agent: string, sessionId: string) => {
      ensure(agent);
      set((draft: any) => {
        const st = draft.agents[agent];
        if (!st) return;
        st.activeSessionIds = st.activeSessionIds.filter((id: string) => id !== sessionId);
      });
    },

    switchBranch: (parentMessageId: string, selectedChildId: string) => {
      const agent = get().currentAgent;
      const st = get().agents[agent];
      if (!st) return;

      set((draft: any) => {
        const s = draft.agents[agent];
        if (s) s.selectedBranches[parentMessageId] = selectedChildId;
      });

      // Re-resolve display messages from cached history rows
      const store = get();
      if (selectIsReplayingHistory(store, agent) && st.activeSessionId) {
        queryClient.invalidateQueries({ queryKey: qk.sessionMessages(st.activeSessionId) });
      }
    },
  };
}
