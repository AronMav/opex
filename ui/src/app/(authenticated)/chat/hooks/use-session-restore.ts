"use client";

import { useEffect, useState, useRef } from "react";
import { useSearchParams } from "next/navigation";
import {
  useChatStore,
  isActivePhase,
  getInitialAgent,
} from "@/stores/chat-store";
import { assertToken } from "@/lib/api";
import type { SessionRow } from "@/types/api";

interface UseSessionRestoreArgs {
  currentAgent: string;
  sessions: SessionRow[];
  sessionsReady: boolean;
  activeSessionId: string | null;
  agents: string[];
}

interface UseSessionRestoreResult {
  effectiveUrlSessionId: string | null;
  setOverrideUrlSession: React.Dispatch<
    React.SetStateAction<string | null | undefined>
  >;
  restoredAgents: React.MutableRefObject<Set<string>>;
}

/**
 * Session-restore state machine extracted verbatim from chat/page.tsx.
 * This is the most fragile part of the chat UI (5-priority restore, cross-agent
 * deep-link resolver, override-state logic). ZERO behavioural changes intended.
 */
export function useSessionRestore({
  currentAgent,
  sessions,
  sessionsReady,
  activeSessionId,
  agents,
}: UseSessionRestoreArgs): UseSessionRestoreResult {
  const searchParams = useSearchParams();
  const urlSessionId = searchParams.get("s");
  // Override state: null = user switched agents (block resolver); undefined = use real searchParams.
  // Set synchronously in switchAgent so it batches with setCurrentAgent in the same render.
  const [overrideUrlSession, setOverrideUrlSession] = useState<string | null | undefined>(undefined);
  const effectiveUrlSessionId =
    overrideUrlSession !== undefined ? overrideUrlSession : urlSessionId;

  // Track which agents have been auto-restored (per-agent, not global boolean)
  // This preserves "new chat" state when switching A → B → A
  const restoredAgents = useRef(new Set<string>());

  // Initialize current agent on mount
  useEffect(() => {
    if (agents.length > 0 && !currentAgent) {
      const initial = getInitialAgent(agents);
      useChatStore.getState().setCurrentAgent(initial);
    }
  }, [agents, currentAgent]);

  // Sync agent state when agents list changes (e.g. after async restore)
  useEffect(() => {
    if (agents.length > 0 && currentAgent && !agents.includes(currentAgent)) {
      useChatStore.getState().setCurrentAgent(agents[0]);
    }
  }, [agents, currentAgent]);

  // Reset override to undefined whenever Next.js router actually navigates
  // (e.g., user clicks a deep-link, router.push). window.history.replaceState
  // does NOT update useSearchParams, so this does not fire during a switch.
  useEffect(() => {
    setOverrideUrlSession(undefined);
  }, [searchParams]);

  // Cross-agent URL deep-link resolver. When ?s= session is not in the current agent's
  // list, fetch the session to find its owning agent and switch to it. This handles
  // shared URLs where the recipient's localStorage points to a different agent.
  //
  // After switching, we DIRECTLY call selectSession(urlSessionId, targetAgent) to
  // honour the deep link without depending on the restoration effect's race-prone
  // state machine. The restoration effect for the new agent will see that
  // activeSessionId is already set + mode=history and become a no-op (the
  // "already viewing a real session — don't touch" branch).
  //
  // Pre-marking restoredAgents prevents the restoration effect from clobbering
  // our deep-link selection if it happens to run between setCurrentAgent and
  // selectSession (e.g. when Arty's sessions list arrives before our state
  // updates have all flushed).
  const urlResolveFetched = useRef<string | null>(null);
  useEffect(() => {
    if (!effectiveUrlSessionId || !sessionsReady || !currentAgent) return;
    const agentState = useChatStore.getState().agents[currentAgent];
    if (agentState?.activeSessionId === effectiveUrlSessionId) return;
    if (sessions.some((s) => s.id === effectiveUrlSessionId)) return; // restore effect handles this
    if (urlResolveFetched.current === effectiveUrlSessionId) return; // already tried
    urlResolveFetched.current = effectiveUrlSessionId;
    fetch(`/api/sessions/${effectiveUrlSessionId}`, {
      headers: { Authorization: `Bearer ${assertToken()}` },
    })
      .then((r) => (r.ok ? r.json() : null))
      .then((data: { agent_id?: string } | null) => {
        if (!data?.agent_id) return;
        const targetAgent = data.agent_id;
        if (!agents.includes(targetAgent)) return;
        restoredAgents.current.add(targetAgent);
        // Same-agent deep-link (I1): e.g. Ctrl+K from /workspace to a session
        // outside currentAgent's loaded window. This resolver only runs when the
        // session is NOT in the current list (guarded above), so the restore
        // effect's "already viewing a real session" branch would strand the user
        // on the OLD session. Select the URL session in place — no agent switch.
        if (targetAgent !== currentAgent) {
          useChatStore.getState().setCurrentAgent(targetAgent);
        }
        useChatStore.getState().selectSession(effectiveUrlSessionId, targetAgent);
      })
      .catch(() => {});
  }, [effectiveUrlSessionId, sessionsReady, sessions, currentAgent, agents]);

  useEffect(() => {
    if (!currentAgent || !sessionsReady) return;

    // Already restored this agent — skip
    if (restoredAgents.current.has(currentAgent)) return;

    const agentState = useChatStore.getState().agents[currentAgent];

    // If the session is still active (streaming/submitted), we must NOT
    // bail out — we need to selectSession + resumeStream to silently
    // reconnect to the in-flight stream and watch the ongoing generation.
    // The only case where we skip is when connectionPhase is active AND
    // messageSource is already showing content (live/finishing/history)
    // — that means the zustand persist already restored a valid view.
    // When messageSource is "new-chat" with an active phase, it's an F5
    // reload mid-stream: the phase was persisted but the content wasn't.
    // Fall through to selectSession below, which calls abortLocalOnly
    // (resets phase to idle properly through the stream lifecycle) and
    // then loads the session + resumes the stream.
    if (isActivePhase(agentState?.connectionPhase)) {
      const mode = agentState?.messageSource?.mode;
      if (mode === "live" || mode === "finishing" || mode === "history") {
        // Already showing a real session — don't touch
        return;
      }
      // F5 mid-stream: fall through to selectSession which will
      // abortLocalOnly (resetting the phase through the proper lifecycle)
      // and then load the session content + resume the stream.
    }

    // If has activeSessionId but UI shows new-chat — WS set the ID but didn't load the session.
    // Load it now.
    if (agentState?.activeSessionId && agentState?.messageSource?.mode === "new-chat") {
      restoredAgents.current.add(currentAgent);
      useChatStore.getState().selectSession(agentState.activeSessionId, currentAgent);
      return;
    }

    // I1-b: an explicit ?s= deep-link to a DIFFERENT session that IS in the
    // loaded window beats the "already viewing" branch below. Without this,
    // that branch marks restored + returns before Priority 1 is ever reached,
    // and the URL-sync effect then rewrites ?s= back to the old session —
    // stranding a same-agent palette jump to a recent session (Ctrl+K from a
    // non-chat page). Same-id keeps current behavior (falls through).
    if (
      effectiveUrlSessionId &&
      effectiveUrlSessionId !== agentState?.activeSessionId &&
      sessions.some((s) => s.id === effectiveUrlSessionId)
    ) {
      restoredAgents.current.add(currentAgent);
      const urlSession = sessions.find((s) => s.id === effectiveUrlSessionId);
      useChatStore.getState().selectSession(effectiveUrlSessionId, currentAgent);
      // If session is still running, mark it so ChatThread's auto-resume effect picks it up
      if (urlSession?.run_status === "running") {
        useChatStore.getState().markSessionActive(currentAgent, effectiveUrlSessionId);
      }
      return;
    }

    // If already viewing a real session (live or history) — validate it still
    // exists in the current sessions list. Pre-populated last session IDs may
    // be stale (deleted or outside the top-40 window); fall through to re-select
    // in that case so the restore effect picks a valid session.
    if (agentState?.activeSessionId && agentState?.messageSource?.mode !== "new-chat") {
      if (sessions.some((s) => s.id === agentState.activeSessionId)) {
        restoredAgents.current.add(currentAgent);
        return;
      }
      // Session not found — fall through to re-select below
    }

    // Priority 1: URL ?s= param (deep link)
    if (effectiveUrlSessionId && sessions.some((s) => s.id === effectiveUrlSessionId)) {
      restoredAgents.current.add(currentAgent);
      const urlSession = sessions.find((s) => s.id === effectiveUrlSessionId);
      useChatStore.getState().selectSession(effectiveUrlSessionId, currentAgent);
      // If session is still running, mark it so ChatThread's auto-resume effect picks it up
      if (urlSession?.run_status === "running") {
        useChatStore.getState().markSessionActive(currentAgent, effectiveUrlSessionId);
      }
      return;
    }

    // IMPORTANT: If effectiveUrlSessionId exists but is NOT in current agent's sessions,
    // it likely belongs to a different agent. Do NOT fall through to Priority 2
    // (most-recent session) — selecting another session here triggers the URL-sync
    // effect to overwrite ?s= with the wrong session id, clobbering the deep link
    // before the cross-agent resolver effect has a chance to switch us to the
    // correct agent. Bail out and let the resolver handle it; deliberately do NOT
    // mark currentAgent as restored so a later visit (without deep link) still
    // restores normally.
    if (effectiveUrlSessionId && !sessions.some((s) => s.id === effectiveUrlSessionId)) {
      return;
    }

    // Priority 2: Most recent session
    if (sessions.length > 0) {
      restoredAgents.current.add(currentAgent);
      useChatStore.getState().selectSession(sessions[0].id, currentAgent);
      if (sessions[0].run_status === "running") {
        useChatStore.getState().markSessionActive(currentAgent, sessions[0].id);
      }
      return;
    }

    restoredAgents.current.add(currentAgent);
    useChatStore.getState().newChat();
  }, [sessionsReady, sessions, currentAgent, effectiveUrlSessionId]);

  // Sync activeSessionId → URL ?s= param.
  //
  // Guard: if effectiveUrlSessionId points to a session NOT in the current agent's
  // sessions list, the cross-agent resolver is likely mid-flight. Don't overwrite
  // ?s= in that window. When overrideUrlSession = null (user switched agents),
  // effectiveUrlSessionId is null and the guard doesn't block — allowing ?s= to
  // update to the new agent's session once restore completes.
  useEffect(() => {
    if (!activeSessionId) return;
    const currentUrlSession = searchParams.get("s");
    if (currentUrlSession === activeSessionId) return;

    if (
      effectiveUrlSessionId &&
      sessions.length > 0 &&
      !sessions.some((s) => s.id === effectiveUrlSessionId)
    ) {
      return; // resolver in flight — don't overwrite
    }

    const url = new URL(window.location.href);
    url.searchParams.set("s", activeSessionId);
    window.history.replaceState(null, "", url.pathname + url.search);
  }, [activeSessionId, searchParams, sessions, effectiveUrlSessionId]);

  return { effectiveUrlSessionId, setOverrideUrlSession, restoredAgents };
}
