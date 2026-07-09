// ── chat/actions/composer.ts ────────────────────────────────────────────────
// Composer/input actions extracted from chat-store.ts.
// Handles thinking level, model override, message loading, and error clearing.

import type { ActionDeps } from "../../chat-store";
import type { AgentState } from "../../chat-types";
import { emptyAgentState } from "../../chat-types";

export function createComposerActions(deps: ActionDeps) {
  const { get, set } = deps;

  // ── Internal helper (mirroring store-level update) ──────────────────────
  function update(agent: string, patch: Partial<AgentState>) {
    set((draft) => {
      if (!draft.agents[agent]) draft.agents[agent] = emptyAgentState();
      Object.assign(draft.agents[agent], patch);
    });
  }

  // ── Composer actions ────────────────────────────────────────────────────

  return {
    clearError: () => {
      const agent = get().currentAgent;
      update(agent, { streamError: null });
    },

    setThinking: (agent: string, sessionId: string | null) => {
      const st = get().agents[agent];
      const updates: Partial<AgentState> = {};

      // On reload (before restore): Zustand activeSessionId is null — set it so
      // useSessionMessages can fetch and the DB streaming record is visible.
      // Guard: only when null AND not in "new chat" mode — don't override newChat().
      if (sessionId !== null && st?.activeSessionId == null && !st?.forceNewSession) {
        updates.activeSessionId = sessionId;
      }

      if (Object.keys(updates).length > 0) update(agent, updates);
    },

    setThinkingLevel: (level: number) => {
      const clampedLevel = Math.max(0, Math.min(5, level));
      get().sendMessage(`/think ${clampedLevel}`);
    },

    setModelOverride: async (agent: string, model: string | null) => {
      // F055: the /api/chat body carries no model field — the backend applies the
      // override solely from this persisted record. An optimistic update that is
      // never rolled back on failure leaves the UI showing a model the backend
      // never uses. Capture the previous value and revert on failure.
      const prev = get().agents[agent]?.modelOverride ?? null;
      update(agent, { modelOverride: model });
      const { getToken } = await import("@/lib/api");
      const token = getToken();
      try {
        const resp = await fetch(`/api/agents/${encodeURIComponent(agent)}/model-override`, {
          method: "POST",
          headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` },
          body: JSON.stringify({ model }),
        });
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      } catch (e) {
        console.warn("[chat] model-override save failed:", e);
        update(agent, { modelOverride: prev });
        const { toast } = await import("sonner");
        toast.error("Не удалось сохранить выбор модели");
      }
    },

    loadEarlierMessages: (agent: string) => {
      set((draft) => {
        const st = draft.agents[agent];
        if (st) st.renderLimit = (st.renderLimit ?? 100) + 100;
      });
    },
  };
}
