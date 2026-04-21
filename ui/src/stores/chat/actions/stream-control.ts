// ── chat/actions/stream-control.ts ──────────────────────────────────────────
// Stream-lifecycle actions extracted from chat-store.ts.
// Receives dependencies via ActionDeps — same get/set closures the immer
// factory provides, plus queryClient and the streaming renderer.

import type { ActionDeps } from "../../chat-store";
import { isActivePhase, emptyAgentState, getLiveMessages } from "../../chat-types";
import type { ChatMessage, TextPart } from "../../chat-types";
import { getCachedHistoryMessages } from "../../chat-history";
import { apiPost } from "@/lib/api";

export function createStreamActions(deps: ActionDeps) {
  const { get, set, renderer } = deps;

  // ── Stream-control actions ───────────────────────────────────────────────

  return {
    sendMessage: (text: string, attachments?: Array<any>) => {
      const store = get();
      const agent = store.currentAgent;
      const st = store.agents[agent] ?? emptyAgentState();

      if (isActivePhase(st.connectionPhase)) return;

      let sessionId = st.activeSessionId;
      let seedMessages: ChatMessage[] = [];

      if (st.messageSource.mode === "history") {
        // Continue from history — get messages from React Query cache.
        // Do NOT flip messageSource here; startStream sets messageSource atomically.
        seedMessages = getCachedHistoryMessages(sessionId, st.selectedBranches);
      } else if (st.messageSource.mode === "live" && st.messageSource.messages.length > 0) {
        seedMessages = st.messageSource.messages;
      }

      renderer.startStream(agent, sessionId, seedMessages, text, attachments);
    },

    stopStream: () => {
      const agent = get().currentAgent;
      // abortActiveStream fires POST /abort to the backend (cancels the pipeline
      // CancellationToken) AND tears down the local SSE connection. Using
      // abortLocalOnly here was a bug (H1): the backend kept processing after the
      // user pressed Stop, wasting LLM tokens and keeping run_status='running'.
      renderer.abortActiveStream(agent);
    },

    resumeStream: (agent: string, sessionId: string) => renderer.resumeStream(agent, sessionId),

    regenerate: () => {
      const store = get();
      const agent = store.currentAgent;
      const st = store.agents[agent] ?? emptyAgentState();

      // Abort any active stream first
      if (isActivePhase(st.connectionPhase)) {
        renderer.abortActiveStream(agent);
      }

      let sessionId = st.activeSessionId;
      let messages: ChatMessage[];

      if (st.messageSource.mode === "history") {
        // Do NOT flip messageSource here; startStream sets messageSource atomically.
        messages = getCachedHistoryMessages(sessionId, st.selectedBranches);
      } else {
        messages = getLiveMessages(st.messageSource);
      }

      // Remove last assistant message
      if (messages.length > 0 && messages[messages.length - 1].role === "assistant") {
        messages = messages.slice(0, -1);
      }

      // Get last user message text
      const lastUser = [...messages].reverse().find((m) => m.role === "user");
      if (!lastUser) return;
      const userText = lastUser.parts
        .filter((p): p is TextPart => p.type === "text")
        .map((p) => p.text)
        .join("\n");

      // Remove last user message too (startStream will re-add it)
      messages = messages.slice(0, messages.lastIndexOf(lastUser));

      renderer.startStream(agent, sessionId, messages, userText);
    },

    regenerateFrom: (messageId: string) => {
      const store = get();
      const agent = store.currentAgent;
      const st = store.agents[agent] ?? emptyAgentState();

      if (isActivePhase(st.connectionPhase)) {
        renderer.abortActiveStream(agent);
      }

      let sessionId = st.activeSessionId;
      let messages: ChatMessage[];

      if (st.messageSource.mode === "history") {
        // Do NOT flip messageSource here; startStream sets messageSource atomically.
        messages = getCachedHistoryMessages(sessionId, st.selectedBranches);
      } else {
        messages = getLiveMessages(st.messageSource);
      }

      // Find the target user message and truncate everything after it
      const targetIdx = messages.findIndex((m) => m.id === messageId);
      if (targetIdx === -1) {
        // Fallback to normal regenerate if message not found
        get().regenerate();
        return;
      }

      const targetMsg = messages[targetIdx];
      if (targetMsg.role !== "user") {
        get().regenerate();
        return;
      }

      const userText = targetMsg.parts
        .filter((p) => p.type === "text")
        .map((p) => (p as { text: string }).text)
        .join("\n");

      // Keep only messages before the target (startStream re-adds the user message)
      const seedMessages = messages.slice(0, targetIdx);

      renderer.startStream(agent, sessionId, seedMessages, userText);
    },

    forkAndRegenerate: async (messageId: string, newContent: string) => {
      const store = get();
      const agent = store.currentAgent;
      const st = store.agents[agent] ?? emptyAgentState();
      const sessionId = st.activeSessionId;
      if (!sessionId) return;

      try {
        const resp = await apiPost<{
          message_id: string;
          parent_message_id: string;
          branch_from_message_id: string;
        }>(`/api/sessions/${sessionId}/fork`, {
          branch_from_message_id: messageId,
          content: newContent,
        });

        const currentSt = get().agents[agent] ?? emptyAgentState();
        let messages: ChatMessage[];
        if (currentSt.messageSource.mode === "history") {
          messages = getCachedHistoryMessages(sessionId, currentSt.selectedBranches);
        } else {
          messages = getLiveMessages(currentSt.messageSource);
        }

        const forkIdx = messages.findIndex((m) => m.id === messageId);
        const seedMessages = forkIdx >= 0 ? messages.slice(0, forkIdx) : messages;

        set((draft: any) => {
          const s = draft.agents[agent];
          if (s && resp.parent_message_id) {
            s.selectedBranches[resp.parent_message_id] = resp.message_id;
          }
        });

        renderer.startStream(agent, sessionId, seedMessages, newContent);
      } catch (e) {
        console.error("[fork] failed:", e);
      }
    },
  };
}
