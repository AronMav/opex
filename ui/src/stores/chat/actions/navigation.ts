// ── chat/actions/navigation.ts ──────────────────────────────────────────────
// Navigation actions extracted from chat-store.ts.
// Receives dependencies via ActionDeps — same get/set closures the immer
// factory provides, plus queryClient and the streaming renderer.

import type { ActionDeps } from "../../chat-store";
import { qk } from "@/lib/queries";
import { saveLastSession, getLastSessionId } from "../../chat-persistence";
import { isActivePhase } from "../../chat-types";
import { selectIsReplayingHistory } from "../../chat-selectors";
import { getTranslations } from "@/i18n";
import { useLanguageStore } from "@/stores/language-store";
import { usePaletteStore } from "@/stores/palette-store";
import { makeUpdate, makeEnsure } from "./_shared";

export function createNavigationActions(deps: ActionDeps) {
  const { get, set, queryClient, renderer } = deps;

  const update = makeUpdate(set);
  const ensure = makeEnsure(get, set);

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

      // Fix H (LOST case): if the departing agent has a queued message, it can
      // never be drained after the switch — setCurrentAgent forces prev's phase
      // to idle AND remounts ChatThread, so the idle-transition edge the drain
      // effect watches for is never observed; returning later inits the effect's
      // ref to an already-idle phase and it never fires → silently stuck forever.
      // Clear it with a visible notice: no silent loss, no misdelivery.
      if (get().agents[prev]?.pendingMessage) {
        set((draft) => {
          const s = draft.agents[prev];
          if (s) s.pendingMessage = null;
        });
        void import("sonner").then(({ toast }) => {
          const translations = getTranslations(useLanguageStore.getState().locale);
          toast.info(translations["chat.queue_discarded_agent_changed"]);
        });
      }

      // MEM-01: clean up all Maps for previous agent.
      // Must happen BEFORE multi-agent reuse check to ensure the previous agent's
      // connectionPhase is set to "idle" and its stream is aborted.
      renderer.cleanupAgent(prev);
      update(prev, { connectionPhase: "idle" });

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
      ensure(name);
      // Pre-populate with last known session so the first render shows content
      // immediately instead of flashing a blank new-chat state while the restore
      // effect runs. The restore effect validates this session and re-selects
      // sessions[0] if it no longer exists.
      const lastSessionId = getLastSessionId(name);
      update(name, {
        activeSessionId: lastSessionId ?? null,
        messageSource: lastSessionId
          ? { mode: "history", sessionId: lastSessionId }
          : { mode: "new-chat" },
        streamError: null,
        connectionPhase: "idle",
        connectionError: null,
        // false = resume existing session; true = backend creates new one on next send
        forceNewSession: !lastSessionId,
      });
      set({ currentAgent: name });
      saveLastSession(name, lastSessionId ?? undefined);
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

      // I2: a manual jump to a DIFFERENT session than a pending palette/scroll
      // target means that target can never resolve here — clear it so it can't
      // fire a surprise delayed jump when its own session is later opened, and
      // so scroll-restore stops yielding to a dead target. The palette's own
      // setTarget→selectSession handoff points at THIS session (same id) and is
      // preserved.
      const pendingTarget = usePaletteStore.getState().target;
      if (pendingTarget && pendingTarget.sessionId !== sessionId) {
        usePaletteStore.getState().setTarget(null);
      }

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
        // Clear per-stream token counts so the ContextBar shows the new
        // session's last_input_tokens from the session list (not stale live values).
        contextTokens: null,
        cacheReadTokens: null,
        cacheCreationTokens: null,
        reasoningTokens: null,
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
      // Captured before the draft mutation below (not reactive inside it) —
      // gates the auto-open branch to the agent the user is actually viewing.
      const isCurrentAgent = agent === get().currentAgent;
      let shouldResume = false;
      let opened = false;
      set((draft) => {
        const st = draft.agents[agent];
        if (!st) return;
        if (!st.activeSessionIds.includes(sessionId)) {
          st.activeSessionIds.push(sessionId);
        }
        if (isActivePhase(st.connectionPhase)) return;

        // Item 1 (2026-07-18): post-login / post-mount restore race. The
        // WS "agent_processing" snapshot can report a running session for
        // the current agent while the UI never explicitly opened one — the
        // welcome screen (messageSource "new-chat") with activeSessionId
        // either still null OR already set to this session (the global
        // setThinking handler in layout.tsx can set activeSessionId ahead of
        // this handler, in either fan-out order, without touching
        // messageSource). Left unfixed, resumeStream flips connectionPhase to
        // active over an empty welcome screen, and ChatPage's restore effect
        // then bails out permanently on its "already streaming — don't
        // touch" guard. Open the session (activeSessionId + messageSource)
        // before resuming. Narrow on purpose: only for the currently-viewed
        // agent, and only when nothing else is already explicitly selected —
        // never hijacks a different session the user opened themselves.
        const isWelcome =
          (st.activeSessionId === null || st.activeSessionId === sessionId) &&
          st.messageSource?.mode === "new-chat";
        if (isCurrentAgent && isWelcome) {
          st.activeSessionId = sessionId;
          st.messageSource = { mode: "history", sessionId };
          shouldResume = true;
          opened = true;
          return;
        }

        // Auto-resume trigger: if the session is already open in the UI and
        // not already streaming, kick off a resume. Idempotent — resumeStream
        // returns 204 if the session is already finalized.
        if (st.activeSessionId === sessionId) {
          shouldResume = true;
        }
      });
      if (opened) saveLastSession(agent, sessionId);
      if (shouldResume) {
        get().resumeStream(agent, sessionId);
      }
    },

    finalizeHandoff: (agent: string, sessionId: string) => {
      const st = get().agents[agent];
      if (!st) return;
      // Only act while a finished turn is still shown as a frozen live/finishing
      // overlay. If the phase is still active, or we already switched to
      // history, this is a no-op (idempotent — the ChatThread effect may fire
      // more than once as the query cache settles).
      if (isActivePhase(st.connectionPhase)) return;
      if (st.messageSource.mode !== "live" && st.messageSource.mode !== "finishing") return;
      update(agent, {
        messageSource: { mode: "history", sessionId },
      });
    },

    markSessionInactive: (agent: string, sessionId: string) => {
      ensure(agent);
      set((draft) => {
        const st = draft.agents[agent];
        if (!st) return;
        st.activeSessionIds = st.activeSessionIds.filter((id: string) => id !== sessionId);
      });
    },

    switchBranch: (parentMessageId: string, selectedChildId: string) => {
      const agent = get().currentAgent;
      const st = get().agents[agent];
      if (!st) return;

      // Fix L: never switch branches while a turn is active for this session.
      // resolveActivePath would re-walk to a different trunk and the live
      // overlay (old branch's lineage) would render after a different branch's
      // history — two unrelated branches blended. The BranchNavigator disables
      // its arrows on the same phase; this is the store-level backstop for any
      // other caller.
      if (isActivePhase(st.connectionPhase)) return;

      set((draft) => {
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
