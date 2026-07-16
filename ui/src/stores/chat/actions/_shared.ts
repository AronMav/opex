// ── chat/actions/_shared.ts ──────────────────────────────────────────────────
// Общие фабрики update/ensure — единственная копия (ранее 4 дубля).
import { emptyAgentState } from "../../chat-types";
import type { AgentState, ChatStore } from "../../chat-types";

type SetFn = (fn: (draft: ChatStore) => void) => void;
type GetFn = () => ChatStore;

export function makeUpdate(set: SetFn) {
  return function update(agent: string, patch: Partial<AgentState>): void {
    set((draft) => {
      if (!draft.agents[agent]) draft.agents[agent] = emptyAgentState();
      Object.assign(draft.agents[agent], patch);
    });
  };
}

export function makeEnsure(get: GetFn, set: SetFn) {
  return function ensure(agent: string): AgentState {
    const s = get().agents[agent];
    if (s) return s;
    const fresh = emptyAgentState();
    // Restore persisted context limit so ContextBar is correct before first SSE.
    try {
      const stored = localStorage.getItem(`ctx_limit:${agent}`);
      if (stored) fresh.modelContextLimit = Number(stored) || null;
    } catch {}
    set((draft) => { draft.agents[agent] = fresh; });
    return fresh;
  };
}
