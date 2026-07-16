"use client";

import { useCallback } from "react";
import { useChatStore, isActivePhase } from "@/stores/chat-store";
import { useWsSubscription } from "@/hooks/use-ws-subscription";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";

/**
 * Chat-page WebSocket subscriptions extracted verbatim from chat/page.tsx.
 * All four handlers read from useChatStore.getState() / event payloads, so the
 * hook needs no arguments. ZERO behavioural changes intended.
 */
export function useChatWs(): void {
  // Refresh session list and currently viewed session when backend finishes processing
  useWsSubscription("session_updated", useCallback(() => {
    const s = useChatStore.getState();
    const agentState = s.agents[s.currentAgent];

    // Always refresh the session list to show latest snippet/timestamp
    queryClient.invalidateQueries({ queryKey: qk.sessions(s.currentAgent) });

    // If we're looking at the updated session, sync our local state with DB
    if (agentState?.activeSessionId) {
      // Invalidate message cache so useSessionMessages() picks up the changes
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(agentState.activeSessionId) });

      // If NOT actively streaming, force a refresh of the history to ensure consistency
      // between live SSE-built state and final DB state.
      if (!isActivePhase(agentState.connectionPhase)) {
        s.refreshHistory(agentState.activeSessionId);
      }
    }
  }, []));

  // Server-driven session status via WS agent_processing events.
  // Backend sends initial state on WS connect, then start/end events in real-time.
  // This updates activeSessionIds in Zustand — the single source of truth for "is session running?".
  useWsSubscription("agent_processing", useCallback((data) => {
    if (!data.session_id) return;
    const store = useChatStore.getState();
    if (data.status === "start") {
      store.markSessionActive(data.agent, data.session_id);
    } else {
      store.markSessionInactive(data.agent, data.session_id);
      // Refetch sessions to get final title, message count, run_status
      queryClient.invalidateQueries({ queryKey: qk.sessions(data.agent) });
    }
  }, []));

  useWsSubscription("file_job_progress", useCallback((data: {
    job_id: string; handler_id: string; session_id: string; phase: string; pct: number; status: string;
  }) => {
    const store = useChatStore.getState();
    if (data.status === "done" || data.status === "failed") {
      store.clearVideoProgress(data.session_id);
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(data.session_id) });
    } else {
      store.setVideoProgress(data.session_id, data.phase, data.phase);
    }
  }, []));

  // Another agent was invited into this session (multi-agent sessions) — keep
  // the participant list in the chat store in sync so the UI can render it.
  useWsSubscription("agent_joined", useCallback((data) => {
    useChatStore.getState().updateSessionParticipants(data.session_id, data.participants);
  }, []));

  // approval_requested handler moved to layout.tsx (must be visible on any page)
}
