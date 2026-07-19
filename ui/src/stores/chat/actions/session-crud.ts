// ── chat/actions/session-crud.ts ─────────────────────────────────────────────
// Session CRUD actions extracted from chat-store.ts.
// Receives dependencies via ActionDeps — same get/set closures the immer
// factory provides, plus queryClient and the streaming renderer.

import type { ActionDeps } from "../../chat-store";
import { getLiveMessages } from "../../chat-types";
import type { AgentState, CompressionDividerPart, ChatMessage } from "../../chat-types";
import type { CompressionEvent, MessagesResponse } from "@/types/api";
import { qk, patchSessionTitleInPages, type SessionsInfiniteData } from "@/lib/queries";
import { apiDelete, apiGet, apiPatch } from "@/lib/api";
import { saveLastSession } from "../../chat-persistence";
import { getCachedHistoryMessages, convertHistory } from "../../chat-history";
import { makeUpdate } from "./_shared";

function insertCompressionDividers(
  messages: ChatMessage[],
  events: CompressionEvent[],
  totalSegments: number,
): ChatMessage[] {
  if (events.length === 0) return messages;
  const dividerMap = new Map(events.map((e) => [e.first_live_message_id, e]));
  const result: ChatMessage[] = [];
  for (const msg of messages) {
    const event = dividerMap.get(msg.id);
    if (event) {
      const dividerPart: CompressionDividerPart = {
        type: "compression-divider",
        segmentIndex: event.segment_index,
        totalSegments,
      };
      result.push({
        id: `compression-divider-${event.segment_index}`,
        role: "assistant",
        parts: [dividerPart],
      });
    }
    result.push(msg);
  }
  return result;
}

export function createSessionCrudActions(deps: ActionDeps) {
  const { get, set, queryClient, renderer } = deps;

  const update = makeUpdate(set);

  // ── Session CRUD actions ─────────────────────────────────────────────────

  return {
    updateSessionParticipants: (sessionId: string, participants: string[]) => {
      set((draft) => {
        draft.sessionParticipants[sessionId] = participants;
      });
    },

    refreshHistory: (sessionId: string, _agentName?: string) => {
      // Invalidate React Query cache — useSessionMessages will re-fetch
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
    },

    renameSession: async (sessionId: string, title: string) => {
      const agent = get().currentAgent;
      await apiPatch(`/api/sessions/${sessionId}?agent=${encodeURIComponent(agent)}`, { title });
      // Patch the title in-place across the infinite cache instead of
      // invalidating: a refetch of all loaded pages would rebuild the array and
      // reset the sidebar's Virtuoso scroll position. The in-place patch keeps
      // untouched pages referentially stable, so scroll stays put.
      queryClient.setQueryData<SessionsInfiniteData>(qk.sessions(agent), (old) =>
        patchSessionTitleInPages(old, sessionId, title),
      );
    },

    deleteSession: async (sessionId: string, skipInvalidation = false) => {
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
      if (!skipInvalidation) {
        queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
      }
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
      const mode = st.messageSource.mode;
      if ((mode === "history" || mode === "finishing") && st.activeSessionId) {
        // "history": invalidate RQ cache to reload without the deleted message.
        // "finishing": the in-progress refetch (stream-processor post-finally) will
        // complete and switch to history mode; just invalidate so the next render
        // shows the correct state. Do NOT reset messageSource here — that would
        // abort the finishing→history state machine.
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

      const liveMessages = getLiveMessages(st.messageSource);
      const messages = liveMessages.length > 0
        ? liveMessages
        : getCachedHistoryMessages(st.activeSessionId, agent, st.selectedBranches);
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

    loadPreviousMessages: async (agentName: string) => {
      const st = get().agents[agentName];
      if (!st || st.isLoadingHistory || !st.hasMoreHistory || !st.activeSessionId) return;

      const liveMessages = getLiveMessages(st.messageSource);
      const firstMsg = liveMessages.find((m) => !m.id.startsWith("compression-divider-"));
      if (!firstMsg) return;

      set((draft) => { draft.agents[agentName].isLoadingHistory = true; });

      try {
        // ?agent= is required server-side (audit 2026-05-08, IDOR fix).
        const params = new URLSearchParams({
          before_id: firstMsg.id,
          limit: "50",
          agent: agentName,
        });
        // C3 fix: go through apiGet (apiFetch under the hood) so the request
        // gets auth handling (401 → /login redirect), a 30s timeout, HTML-body
        // protection, and a single source of truth for token errors. The raw
        // fetch here used to silently throw a SyntaxError on `.json()` when
        // the token expired (backend returned the HTML login page) and left
        // `isLoadingHistory=true` forever if the network stalled.
        const res = await apiGet<MessagesResponse>(
          `/api/sessions/${st.activeSessionId}/messages?${params.toString()}`,
        );

        const converted = convertHistory(res.messages ?? []);
        // segment_count comes from the session record; fall back to 1.
        // (sessionSegmentCount is an optional, currently-unset extension field
        // on AgentState — read defensively rather than via `any`.)
        const totalSegments = (get().agents[agentName] as AgentState & { sessionSegmentCount?: number })?.sessionSegmentCount ?? 1;
        const withDividers = insertCompressionDividers(converted, res.compression_events ?? [], totalSegments);

        const currentLive = getLiveMessages(get().agents[agentName]?.messageSource ?? { mode: "new-chat" });
        update(agentName, {
          messageSource: { mode: "live", messages: [...withDividers, ...currentLive] },
          hasMoreHistory: res.has_more ?? false,
          isLoadingHistory: false,
        });
      } catch (_e) {
        const { toast } = await import("sonner");
        toast.error("Не удалось загрузить историю сообщений");
        set((draft) => { draft.agents[agentName].isLoadingHistory = false; });
      }
    },
  };
}
