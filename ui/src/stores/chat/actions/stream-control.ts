// ── chat/actions/stream-control.ts ──────────────────────────────────────────
// Stream-lifecycle actions extracted from chat-store.ts.
// Receives dependencies via ActionDeps — same get/set closures the immer
// factory provides, plus queryClient and the streaming renderer.

import type { ActionDeps } from "../../chat-store";
import { isActivePhase, emptyAgentState, getLiveMessages, uuid } from "../../chat-types";
import type { ChatMessage, MessageAttachment, TextPart } from "../../chat-types";
import { getCachedHistoryMessages } from "../../chat-history";
import { apiPost } from "@/lib/api";

export function createStreamActions(deps: ActionDeps) {
  const { get, set, renderer } = deps;

  // F085: agents with an interruptAndSend in flight. abortLocalOnly flips
  // connectionPhase to 'idle' synchronously, so a rapid second sendMessage would
  // read phase='idle', take the non-interrupt branch, and start a racing stream
  // that the delayed first startStream then tears down (a dropped/reordered
  // message). The phase alone is not a reliable concurrency gate; this flag is.
  const interrupting = new Set<string>();

  // ── Stream-control actions ───────────────────────────────────────────────

  return {
    sendMessage: (text: string, attachments?: Array<MessageAttachment>) => {
      const store = get();
      const agent = store.currentAgent;
      const st = store.agents[agent] ?? emptyAgentState();

      // An interrupt is already in flight for this agent — queue this message
      // into pendingMessage (drained by ChatThread when the phase reaches idle)
      // instead of racing a fresh startStream (F085).
      if (interrupting.has(agent)) {
        get().queueMessage(text, attachments);
        return;
      }

      // If streaming is active, interrupt and send instead of silently dropping the message.
      if (isActivePhase(st.connectionPhase)) {
        // Fire-and-forget: interruptAndSend is async but sendMessage is sync by interface.
        // We call it without awaiting — the caller can use interruptAndSend directly for
        // explicit async control.
        get().interruptAndSend(text, attachments);
        return;
      }

      const sessionId = st.activeSessionId;
      let seedMessages: ChatMessage[] = [];

      if (st.messageSource.mode === "history") {
        // Continue from history — get messages from React Query cache.
        // Do NOT flip messageSource here; startStream sets messageSource atomically.
        seedMessages = getCachedHistoryMessages(sessionId, st.selectedBranches);
      } else {
        const liveMsgs = getLiveMessages(st.messageSource);
        if (liveMsgs.length > 0) seedMessages = liveMsgs;
      }

      renderer.startStream(agent, sessionId, seedMessages, text, attachments, uuid());
    },

    interruptAndSend: async (text: string, attachments?: Array<MessageAttachment>) => {
      const store = get();
      const agent = store.currentAgent;

      // Mark this agent as interrupting so a rapid follow-up sendMessage queues
      // instead of racing (F085). The add runs synchronously before the first
      // await, so it is already set when sendMessage returns.
      interrupting.add(agent);
      try {
        // Abort the current stream (POST /abort + local teardown).
        renderer.abortActiveStream(agent);

        // Poll up to 1500ms for connectionPhase to reach idle.
        const POLL_INTERVAL_MS = 100;
        const MAX_WAIT_MS = 1500;
        const deadline = Date.now() + MAX_WAIT_MS;
        while (Date.now() < deadline) {
          await new Promise<void>((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
          const phase = get().agents[agent]?.connectionPhase;
          if (!phase || phase === "idle") break;
        }

        // Send regardless of whether we reached idle (timeout safety).
        const currentSt = get().agents[agent] ?? emptyAgentState();
        const sessionId = currentSt.activeSessionId;
        let seedMessages: ChatMessage[] = [];

        if (currentSt.messageSource.mode === "history") {
          seedMessages = getCachedHistoryMessages(sessionId, currentSt.selectedBranches);
        } else {
          const liveMsgs = getLiveMessages(currentSt.messageSource);
          if (liveMsgs.length > 0) seedMessages = liveMsgs;
        }

        renderer.startStream(agent, sessionId, seedMessages, text, attachments, uuid());
      } finally {
        // startStream has (synchronously) re-armed the stream; clearing the flag
        // now lets the queued follow-up drain via ChatThread's idle-phase effect.
        interrupting.delete(agent);
      }
    },

    queueMessage: (text: string, attachments?: Array<MessageAttachment>, opts?: { voice?: boolean }) => {
      const agent = get().currentAgent;
      set((draft) => {
        if (!draft.agents[agent]) draft.agents[agent] = emptyAgentState();
        const prev = draft.agents[agent].pendingMessage;
        // If a previous voice message is already queued and this one is also
        // voice, append with "\n" — the user spoke several phrases during the
        // same turn instead of replacing the earlier one.
        const content = prev?.voice && opts?.voice ? `${prev.content}\n${text}` : text;
        draft.agents[agent].pendingMessage = { content, attachments, voice: opts?.voice ?? prev?.voice };
      });
    },

    clearPending: (agent?: string) => {
      const targetAgent = agent ?? get().currentAgent;
      set((draft) => {
        if (draft.agents[targetAgent]) {
          draft.agents[targetAgent].pendingMessage = null;
        }
      });
    },

    setVoiceTurnPending: (pending: boolean, agent?: string) => {
      const targetAgent = agent ?? get().currentAgent;
      set((draft) => {
        if (!draft.agents[targetAgent]) draft.agents[targetAgent] = emptyAgentState();
        draft.agents[targetAgent].voiceTurnPending = pending;
      });
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

      const sessionId = st.activeSessionId;
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

      const sessionId = st.activeSessionId;
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
        }>(`/api/sessions/${sessionId}/fork?agent=${encodeURIComponent(agent)}`, {
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

        set((draft) => {
          const s = draft.agents[agent];
          if (s && resp.parent_message_id) {
            s.selectedBranches[resp.parent_message_id] = resp.message_id;
          }
        });

        // Pass resp.message_id so the backend reuses the already-persisted branch
        // user message instead of creating a duplicate via /api/chat.
        renderer.startStream(agent, sessionId, seedMessages, newContent, undefined, resp.message_id);
      } catch (e) {
        // F084: surface the failure — a silent console.error leaves the composer
        // looking idle so the user can't tell the edit-and-regenerate failed.
        console.error("[fork] failed:", e);
        const { toast } = await import("sonner");
        toast.error("Не удалось изменить и перегенерировать сообщение");
      }
    },
  };
}
