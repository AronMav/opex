"use client";

import { useChatStore } from "@/stores/chat-store";
import { isActivePhase } from "@/stores/chat-types";

// Stable empty fallback — prevents new array reference on every render
// when activeSessionIds is absent.
const EMPTY_ACTIVE_IDS: string[] = [];

/**
 * Single source of truth for "is the engine processing for this agent?".
 *
 * Combines two signals:
 *  - UI-side connectionPhase (store)
 *  - activeSessionIds (store) — WS-delivered "agent_processing" events
 *
 * After the backend fix (spec 2026-05-13-session-lifecycle-root-fix), DB
 * `run_status` is no longer consulted in the hot path — WS push is the
 * single source of truth. On WS disconnect, the UI may show idle while
 * a session is still running on the backend; this is acceptable and
 * documented as a known limitation.
 */
export function useEngineRunning(agent: string): boolean {
  const activeSessionId = useChatStore((s) => s.agents[agent]?.activeSessionId ?? null);
  const connectionPhase = useChatStore((s) => s.agents[agent]?.connectionPhase ?? "idle");
  const activeSessionIds = useChatStore(
    (s) => s.agents[agent]?.activeSessionIds ?? EMPTY_ACTIVE_IDS,
  );

  return !!activeSessionId && (
    isActivePhase(connectionPhase) ||
    activeSessionIds.includes(activeSessionId)
  );
}
