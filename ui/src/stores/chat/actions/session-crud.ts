// ── chat/actions/session-crud.ts ─────────────────────────────────────────────
// Session CRUD actions extracted from chat-store.ts.
// Receives dependencies via ActionDeps — same get/set closures the immer
// factory provides, plus queryClient and the streaming renderer.

import type { ActionDeps } from "../../chat-store";
import { emptyAgentState, getLiveMessages } from "../../chat-types";
import type { AgentState } from "../../chat-types";
import { qk } from "@/lib/queries";
import { apiDelete, apiPatch } from "@/lib/api";
import { saveLastSession } from "../../chat-persistence";
import { getCachedHistoryMessages } from "../../chat-history";
import { selectIsReplayingHistory } from "../../chat-selectors";

export function createSessionCrudActions(deps: ActionDeps) {
  const { get, set, queryClient, renderer } = deps;

  // ── Internal helpers (mirroring store-level update) ─────────────────────

  function update(agent: string, patch: Partial<AgentState>) {
    set((draft: any) => {
      if (!draft.agents[agent]) draft.agents[agent] = emptyAgentState();
      Object.assign(draft.agents[agent], patch);
    });
  }

  // ── Session CRUD actions ─────────────────────────────────────────────────

  return {
    updateSessionParticipants: (sessionId: string, participants: string[]) => {
      set((draft: any) => {
        draft.sessionParticipants[sessionId] = participants;
      });
    },

    refreshHistory: (sessionId: string, _agentName?: string) => {
      // Invalidate React Query cache — useSessionMessages will re-fetch
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
    },

    renameSession: async (sessionId: string, title: string) => {
      const agent = get().currentAgent;
      await apiPatch(`/api/sessions/${sessionId}`, { title });
      queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
    },

    deleteSession: async (sessionId: string) => {
      const agent = get().currentAgent;
      await apiDelete(`/api/sessions/${sessionId}?agent=${encodeURIComponent(agent)}`);
      const st = get().agents[agent];
      if (st?.activeSessionId === sessionId) {
        // Use captured `agent` — currentAgent may have changed during await
        renderer.abortActiveStream(agent);
        update(agent, {
          activeSessionId: null, messageSource: { mode: "new-chat" },
          streamError: null,
          connectionPhase: "idle", connectionError: null,
          forceNewSession: true,
        });
        saveLastSession(agent);
      }
      queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
    },

    deleteAllSessions: async () => {
      const agent = get().currentAgent;
      await apiDelete(`/api/sessions?agent=${encodeURIComponent(agent)}`);
      // Use captured `agent` — currentAgent may have changed during await
      renderer.abortActiveStream(agent);
      update(agent, {
        activeSessionId: null, messageSource: { mode: "new-chat" },
        streamError: null,
        connectionPhase: "idle", connectionError: null,
        forceNewSession: true,
      });
      saveLastSession(agent);
      queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
    },

    deleteMessage: async (messageId: string) => {
      const agent = get().currentAgent;
      await apiDelete(`/api/messages/${messageId}?agent=${encodeURIComponent(agent)}`);
      const st = get().agents[agent];
      if (!st) return;
      const store = get();
      if (selectIsReplayingHistory(store, agent) && st.activeSessionId) {
        // Invalidate React Query cache to reload history
        queryClient.invalidateQueries({ queryKey: qk.sessionMessages(st.activeSessionId) });
      } else {
        const currentMessages = getLiveMessages(st.messageSource);
        update(agent, {
          messageSource: { mode: "live", messages: currentMessages.filter((m) => m.id !== messageId) },
        });
      }
    },

    exportSession: async () => {
      const store = get();
      const agent = store.currentAgent;
      const st = store.agents[agent];
      if (!st) return;

      const messages = st.messageSource.mode === "live"
        ? st.messageSource.messages
        : getCachedHistoryMessages(st.activeSessionId, st.selectedBranches);
      if (messages.length === 0) return;

      const session = {
        id: st.activeSessionId ?? "unknown",
        agent_id: agent,
        user_id: "",
        channel: "web",
        started_at: messages[0]?.createdAt ?? new Date().toISOString(),
        last_message_at: new Date().toISOString(),
      };

      const { sessionToMarkdown } = await import("@/lib/format");
      const markdown = sessionToMarkdown(messages, session as import("@/types/api").SessionRow, agent);

      const blob = new Blob([markdown], { type: "text/markdown;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      try {
        const a = document.createElement("a");
        a.href = url;
        a.download = `${agent}-${new Date().toISOString().slice(0, 10)}.md`;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
      } finally {
        URL.revokeObjectURL(url);
      }
    },
  };
}
