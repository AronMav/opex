// ── chat/actions/stream-control.ts ──────────────────────────────────────────
// Stream-lifecycle actions extracted from chat-store.ts.
// Receives dependencies via ActionDeps — same get/set closures the immer
// factory provides, plus queryClient and the streaming renderer.

import type { ActionDeps } from "../../chat-store";
import { isActivePhase, emptyAgentState, getLiveMessages, uuid } from "../../chat-types";
import type { AgentState, ChatMessage, MessageAttachment, TextPart } from "../../chat-types";
import { getCachedHistoryMessages } from "../../chat-history";
import { apiPost } from "@/lib/api";
import { qk } from "@/lib/queries";

export function createStreamActions(deps: ActionDeps) {
  const { get, set, renderer, queryClient } = deps;

  // ── Shared fork-and-stream flow (B/C/F) ──────────────────────────────────
  // The ONE correct way to replace-via-branch: POST /fork to create a real
  // sibling user message under branch_from's parent, select that branch, then
  // stream a fresh turn from it. regenerate / regenerateFrom / forkAndRegenerate
  // all funnel through here so they inherit the proven branch semantics instead
  // of appending a forward child (the old CRITICAL bug B: permanent duplicate
  // turn on a flat trunk).
  //
  // - C: abort with abortLocalOnly (NOT abortActiveStream) — POST /abort would
  //   race the fresh turn's register_with_token on the same session key and can
  //   cancel it. register_with_token supersedes the old stream server-side, so
  //   local teardown is sufficient. Guarded by isActivePhase: only aborts when a
  //   stream is actually running.
  // - F: invalidate sessionMessages BEFORE sendTurn so the refetched history
  //   reflects the fork (resolveActivePath auto-selects the newest sibling; the
  //   selectedBranches entry set below pins it) before the live overlay renders.
  async function forkAndStream(
    agent: string,
    sessionId: string,
    branchFromMessageId: string,
    content: string,
    opts?: { model?: string },
  ) {
    const st = get().agents[agent];
    if (st && isActivePhase(st.connectionPhase)) {
      renderer.abortLocalOnly(agent);
    }

    try {
      const resp = await apiPost<{
        message_id: string;
        parent_message_id: string;
        branch_from_message_id: string;
      }>(`/api/sessions/${sessionId}/fork?agent=${encodeURIComponent(agent)}`, {
        branch_from_message_id: branchFromMessageId,
        content,
      });

      set((draft) => {
        const s = draft.agents[agent];
        if (s && resp.parent_message_id) {
          s.selectedBranches[resp.parent_message_id] = resp.message_id;
        }
      });

      // F: history must reflect the fork before the turn streams.
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });

      // Pass resp.message_id (userMessageId) so the backend reuses the
      // already-persisted branch user message instead of minting a duplicate
      // forward child via /api/chat. sendTurn resolves the branch tip
      // (leaf_message_id) from the freshly-selected branch itself.
      void renderer.sendTurn(agent, sessionId, content, {
        userMessageId: resp.message_id,
        model: opts?.model,
      });
    } catch (e) {
      // F084: surface the failure — a silent console.error leaves the composer
      // looking idle so the user can't tell the regenerate/fork failed.
      console.error("[fork] failed:", e);
      const { toast } = await import("sonner");
      toast.error("Не удалось перегенерировать сообщение");
    }
  }

  // ── WS2: persisted branch-id resolution ──────────────────────────────────
  // A regenerate/retry must never hand the fork endpoint a client-only
  // (never-persisted) message id — Task 3 added a server-side fallback for
  // this (unknown id → last persisted message in the session), but that
  // fallback ignores which BRANCH the client is actually viewing. Resolving
  // the anchor here, against the branch-resolved history + live overlay the
  // user is looking at, is strictly better than deferring to the server.

  /** A user message's optimistic echo is "sending" until POST succeeds and
   * "failed" if the POST itself errored — neither has necessarily reached the
   * DB. Every other value (undefined = history row, "confirmed" = acked via
   * data-session-id) means the row is known to exist server-side. */
  function isPersistedStatus(status: ChatMessage["status"]): boolean {
    return status !== "sending" && status !== "failed";
  }

  /** Id-keyed merge of persisted history with the live turn overlay — same
   * "live wins for a shared id, live-only appends" semantics as the render
   * merge (chat-selectors.ts mergeRender), duplicated locally to avoid a
   * circular import (chat-selectors imports chat-store). Used ONLY to look
   * PAST the current (possibly unconfirmed) live turn for a persisted anchor. */
  function mergeForBranchLookup(history: ChatMessage[], live: ChatMessage[]): ChatMessage[] {
    const liveIds = new Set(live.map((m) => m.id));
    return [...history.filter((m) => !liveIds.has(m.id)), ...live];
  }

  /** Walk backward from `fromIndex` (inclusive) for the newest USER message
   * whose id is known-persisted. Returns undefined if none is found — the
   * caller then sends a plain turn instead of forking (nothing to branch
   * from server-side either). */
  function resolvePersistedUserId(messages: ChatMessage[], fromIndex: number): string | undefined {
    for (let i = fromIndex; i >= 0; i--) {
      const m = messages[i];
      if (m.role === "user" && isPersistedStatus(m.status)) return m.id;
    }
    return undefined;
  }

  /** Combined branch-resolved history + live overlay for `agent`/`sessionId` —
   * the same messages regenerate/regenerateFrom reason about. */
  function branchLookupMessages(agent: string, sessionId: string, st: AgentState): ChatMessage[] {
    const historyMessages = getCachedHistoryMessages(sessionId, agent, st.selectedBranches);
    if (st.messageSource.mode === "history") return historyMessages;
    return mergeForBranchLookup(historyMessages, getLiveMessages(st.messageSource));
  }

  /** Fork from `branchId` when persisted; otherwise there is nothing valid to
   * branch from (e.g. the very first turn of a session failed before ever
   * reaching the server) — send a plain new turn instead of a fork request. */
  function regenerateViaResolvedAnchor(
    agent: string,
    sessionId: string,
    branchId: string | undefined,
    content: string,
    opts?: { model?: string },
  ) {
    if (branchId) {
      void forkAndStream(agent, sessionId, branchId, content, opts);
    } else {
      void renderer.sendTurn(agent, sessionId, content, { userMessageId: uuid(), model: opts?.model });
    }
  }

  // F085: agents with an interruptAndSend in flight. abortLocalOnly flips
  // connectionPhase to 'idle' synchronously, so a rapid second sendMessage would
  // read phase='idle', take the non-interrupt branch, and start a racing stream
  // that the delayed first sendTurn then tears down (a dropped/reordered
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
      // instead of racing a fresh sendTurn (F085).
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

      // Single send path (T7): sendTurn writes the optimistic echo, POSTs the
      // turn, and opens the GET envelope stream on the session id from the 202.
      void renderer.sendTurn(agent, st.activeSessionId, text, { attachments, userMessageId: uuid() });
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

        // Send regardless of whether we reached idle (timeout safety). Same
        // single send path as sendMessage.
        const currentSt = get().agents[agent] ?? emptyAgentState();
        void renderer.sendTurn(agent, currentSt.activeSessionId, text, { attachments, userMessageId: uuid() });
      } finally {
        // sendTurn has (synchronously) written the optimistic echo and flipped
        // the phase to "submitted" before its first await; clearing the flag now
        // lets the queued follow-up drain via ChatThread's idle-phase effect.
        interrupting.delete(agent);
      }
    },

    queueMessage: (text: string, attachments?: Array<MessageAttachment>, opts?: { voice?: boolean }) => {
      const agent = get().currentAgent;
      set((draft) => {
        if (!draft.agents[agent]) draft.agents[agent] = emptyAgentState();
        const prev = draft.agents[agent].pendingMessage;
        const isVoice = opts?.voice === true;
        // If a previous voice message is already queued and this one is also
        // voice, append with "\n" — the user spoke several phrases during the
        // same turn instead of replacing the earlier one. A NON-voice queue call
        // (e.g. Shift+Enter, or the F085 interrupt-race path) must NOT inherit a
        // prior pending voice flag — a typed message supersedes a queued voice
        // one and must not be read aloud once sent.
        const content = prev?.voice && isVoice ? `${prev.content}\n${text}` : text;
        // Fix H: stamp the target session + agent so the ChatThread drain can
        // verify the context still matches before sending (no silent loss / no
        // misdelivery on agent- or session-switch).
        draft.agents[agent].pendingMessage = {
          content,
          attachments,
          voice: isVoice,
          sessionId: draft.agents[agent].activeSessionId ?? null,
          agent,
        };
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

    // Refresh / mount / drop-recovery all re-enter through the SAME single
    // connect point as the post-POST path (T7).
    resumeStream: (agent: string, sessionId: string) => renderer.connect(agent, sessionId),

    // B/C/F: regenerate replaces the last answer by BRANCHING from the last
    // user message (real sibling), not by appending a forward child. Routes
    // through the shared forkAndStream flow — same proven path as
    // forkAndRegenerate, minus the edit.
    regenerate: (opts?: { model?: string }) => {
      const store = get();
      const agent = store.currentAgent;
      const st = store.agents[agent] ?? emptyAgentState();

      const sessionId = st.activeSessionId;
      if (!sessionId) return;

      const messages = branchLookupMessages(agent, sessionId, st);

      // Last user message anchors the branch (content to resend).
      let lastUserIdx = -1;
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role === "user") { lastUserIdx = i; break; }
      }
      if (lastUserIdx === -1) return;
      const lastUser = messages[lastUserIdx];
      const userText = lastUser.parts
        .filter((p): p is TextPart => p.type === "text")
        .map((p) => p.text)
        .join("\n");

      // WS2: the leaf user message itself may be an unconfirmed optimistic
      // echo (failed turn) — walk back for the newest PERSISTED anchor
      // instead of blindly trusting lastUser.id.
      const branchId = resolvePersistedUserId(messages, lastUserIdx);
      regenerateViaResolvedAnchor(agent, sessionId, branchId, userText, opts);
    },

    // B/C/F: branch from the given message. If it's a user message, branch from
    // it directly; if it's an assistant message, branch from its nearest
    // PRECEDING user message (its text). Same forkAndStream flow as regenerate.
    regenerateFrom: (messageId: string, opts?: { model?: string }) => {
      const store = get();
      const agent = store.currentAgent;
      const st = store.agents[agent] ?? emptyAgentState();

      const sessionId = st.activeSessionId;
      if (!sessionId) return;

      const messages = branchLookupMessages(agent, sessionId, st);

      const targetIdx = messages.findIndex((m) => m.id === messageId);
      if (targetIdx === -1) {
        // Fallback to normal regenerate if message not found.
        get().regenerate(opts);
        return;
      }

      // Resolve the anchoring USER message: the target itself if it's a user
      // message, else the nearest preceding user message.
      let userIdx = -1;
      for (let i = targetIdx; i >= 0; i--) {
        if (messages[i].role === "user") { userIdx = i; break; }
      }
      if (userIdx === -1) {
        get().regenerate(opts);
        return;
      }
      const userMsg = messages[userIdx];

      const userText = userMsg.parts
        .filter((p): p is TextPart => p.type === "text")
        .map((p) => p.text)
        .join("\n");

      // WS2: same persisted-anchor resolution as regenerate() — the target
      // (or its preceding user message) may itself be an unconfirmed
      // optimistic echo.
      const branchId = resolvePersistedUserId(messages, userIdx);
      regenerateViaResolvedAnchor(agent, sessionId, branchId, userText, opts);
    },

    forkAndRegenerate: async (messageId: string, newContent: string) => {
      const store = get();
      const agent = store.currentAgent;
      const st = store.agents[agent] ?? emptyAgentState();
      const sessionId = st.activeSessionId;
      if (!sessionId) return;

      await forkAndStream(agent, sessionId, messageId, newContent);
    },
  };
}
