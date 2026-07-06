"use client";

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { useChatStore } from "@/stores/chat-store";
import { selectRenderMessages } from "@/stores/chat-selectors";
import type { ChatMessage, ChatState } from "@/stores/chat-types";
import { qk } from "@/lib/queries";

/**
 * Subscribes to the underlying stable fields (not the derived array)
 * and memoizes the result. `selectRenderMessages` creates a fresh
 * array on every call (`[]` / `mergeLiveOverlay(...)`), so passing it
 * to `useChatStore` directly as a selector causes an infinite render
 * loop: Zustand's `Object.is` comparison of the returned reference
 * against the previous one is always false, triggering a re-render
 * that runs the selector again.
 *
 * Fix: subscribe only to the primitive/object inputs whose identity
 * is stable when unchanged (Immer preserves references on no-op
 * writes), and compute the derived array through `useMemo`. The
 * memo's dependency list changes only when the actual inputs do.
 */
export function useRenderMessages(agent: string): ChatMessage[] {
  const messageSource = useChatStore((s) => s.agents[agent]?.messageSource);
  const selectedBranches = useChatStore((s) => s.agents[agent]?.selectedBranches);
  const activeSessionId = useChatStore((s) => s.agents[agent]?.activeSessionId ?? null);

  // Read-only RQ subscription: re-render when the cache for this session
  // is populated by ChatThread's useSessionMessages. staleTime + disabled
  // refetch flags guarantee this hook never initiates a fetch itself.
  // Key must match useSessionMessages exactly (4-element: [...prefix, agent])
  // so dataUpdatedAt fires when new messages arrive.
  const { dataUpdatedAt } = useQuery({
    queryKey: [...qk.sessionMessages(activeSessionId!), agent],
    enabled: !!activeSessionId && !!agent,
    staleTime: Infinity,
    refetchOnMount: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });

  return useMemo(() => {
    // Guard: agent slot not yet initialised — return stable empty array.
    if (!messageSource) return [];

    // Rebuild the selector's inputs into a minimal fake state so we
    // can reuse its logic without duplication. The selector only
    // reads these three fields from `state.agents[agent]`.
    const fakeState = {
      agents: {
        [agent]: {
          messageSource,
          selectedBranches,
          activeSessionId,
        },
      },
    } as unknown as ChatState;
    // Reference dataUpdatedAt so the dependency is genuinely "used" — it is
    // an intentional re-render trigger (RQ bumps it when the session cache is
    // filled by ChatThread's useSessionMessages).
    void dataUpdatedAt;
    return selectRenderMessages(fakeState, agent);
    // messageSource, selectedBranches, activeSessionId are the only
    // inputs that can influence the result. All three have stable
    // identity across renders when their values do not change
    // (Immer draft). dataUpdatedAt triggers recomputation when
    // ChatThread's useSessionMessages fills the RQ cache for this session.
  }, [messageSource, selectedBranches, activeSessionId, agent, dataUpdatedAt]);
}
