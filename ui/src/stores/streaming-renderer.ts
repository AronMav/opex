// ── streaming-renderer.ts ──────────────────────────────────────────────────
// Factory module encapsulating SSE stream processing, rAF throttling,
// reconnection logic, and per-agent cleanup (MEM-01, PERF-02).

import { startTurn, openTurnStream } from "./stream/chat-stream";
import { apiPatch, apiPost } from "@/lib/api";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";

import {
  uuid,
  emptyAgentState,
} from "./chat-types";
import type {
  ChatMessage,
  MessagePart,
  MessageAttachment,
  AgentState,
  ChatStore,
} from "./chat-types";
import { getCachedRawMessages, resolveActivePath } from "./chat-history";
import { streamSessionManager } from "./stream-session";

// ── Store access interface ─────────────────────────────────────────────────
// Typed against the ChatStore shape (type-only import — erased at runtime, so
// there is no circular module dependency with chat-store.ts).

export interface StoreAccess {
  get: () => ChatStore;
  set: (fn: (draft: ChatStore) => void) => void;
}

// ── Factory ────────────────────────────────────────────────────────────────

export function createStreamingRenderer(store: StoreAccess) {
  // ── CLN-02: Encapsulated non-serializable state ──────────────────────────
  // AbortController lives inside StreamSession.signal (Task 3.6). The transport
  // reconnect timers were removed in T8 — a dropped stream is re-opened only via
  // the single `connect` path (staleness-gated on visibility change).

  // Set by dispose(): stops the visibilitychange handler and blocks any new
  // debounce timers from being scheduled after teardown.
  let _disposed = false;

  // Track the last time we saw any SSE traffic per agent. Browsers (especially
  // Chrome) throttle / suspend SSE sockets on hidden tabs; when the tab returns
  // we may have a half-open connection where reader.read() is parked and no
  // events are coming. The visibilitychange listener below re-opens (via
  // `connect`) any agent whose stream has been silent for more than
  // VISIBILITY_STALE_MS while in an active phase.
  const _lastEventTime = new Map<string, number>();
  // T8: tightened from 30_000 → 15_000. The single connect path returns an
  // empty envelope cheaply when there is no in-flight turn, so a shorter
  // staleness window recovers a suspended socket faster with negligible cost.
  const VISIBILITY_STALE_MS = 15_000;

  function recordEventActivity(agent: string): void {
    _lastEventTime.set(agent, Date.now());
  }

  // ── Internal helpers ────────────────────────────────────────────────────

  function update(agent: string, patch: Partial<AgentState>) {
    store.set((draft) => {
      if (!draft.agents[agent]) draft.agents[agent] = emptyAgentState();
      Object.assign(draft.agents[agent], patch);
    });
  }

  // ── Debounced UI state persistence to server ──────────────────────────────
  const uiStateSaveTimers: Record<string, ReturnType<typeof setTimeout>> = {};
  function saveUiState(agent: string) {
    clearTimeout(uiStateSaveTimers[agent]);
    if (_disposed) return; // torn down — don't schedule an orphan timer
    uiStateSaveTimers[agent] = setTimeout(() => {
      const st = store.get().agents[agent];
      if (!st?.activeSessionId) return;
      apiPatch(`/api/sessions/${st.activeSessionId}?agent=${encodeURIComponent(agent)}`, {
        ui_state: { connectionPhase: st.connectionPhase },
      }).catch((e: unknown) => { console.warn("[chat] save failed:", e); });
    }, 500);
  }

  // ── Stream lifecycle ────────────────────────────────────────────────────

  /** Internal: local abort only (no backend notification). Used by
   * sendTurn/connect to clean up lingering fetch controllers before launching
   * a new stream on the same agent. Calling /abort here would race with
   * the new stream's registration on the same session id and cancel it
   * prematurely.
   *
   * cleanupAgent() delegates to this same path during agent switch.
   */
  function abortLocalOnly(agent: string) {
    streamSessionManager.disposeCurrent(agent);
    // `dispose()` lands the final `connectionPhase: "idle"` write and
    // bumps `streamGeneration` atomically. No direct store mutation
    // here — the grep guard (Task 3.8) enforces that stream-state
    // fields are never touched outside StreamSession.
    // Defensive: if the session was already disposed (no-op above), the agent
    // state may still carry an active connectionPhase from a setTimer-pending
    // state. Force-reset to idle so the user never sees a stuck streaming badge
    // after pressing Stop.
    const st = store.get().agents[agent];
    if (st && (st.connectionPhase === "submitted" || st.connectionPhase === "streaming")) {
      store.set((draft) => {
        const a = draft.agents[agent];
        if (!a) return;
        a.connectionPhase = "idle";
        a.connectionError = null;
        a.isLlmReconnecting = false;
      });
    }
  }

  /** Public: abort active stream AND notify backend (user Stop).
   *
   * Fire-and-forget POST /api/chat/{sid}/abort trips the backend's
   * CancellationToken, which cascades through `stream_with_cancellation`
   * into `LlmCallError::UserCancelled { partial_text }`. The engine's
   * error path then persists an aborted message row with
   * `abort_reason='user_cancelled'` and writes an aborted usage_log.
   *
   * The /abort POST fires whenever an `activeSessionId` is known, even
   * if the local AbortController is already gone (network tear-down,
   * SSE auto-reconnect race). This matters because the backend stream
   * may still be registered under the sessionId while the UI has
   * already disposed of its fetch — without this decoupling, user Stop
   * becomes a silent no-op server-side and the streaming row stays
   * `status='streaming'` until the engine finishes naturally.
   *
   * `abortLocalOnly` is a no-op if there is no controller; safe to call.
   */
  function abortActiveStream(agent: string) {
    const sid = store.get().agents[agent]?.activeSessionId;
    if (sid) {
      apiPost(`/api/chat/${sid}/abort?agent=${encodeURIComponent(agent)}`).catch(() => {
        // Backend may not have an active stream (already done / not started).
        // Local abort below still cleans up UI state.
      });
    }
    abortLocalOnly(agent);
  }

  // ── Server-authoritative single connect path (T7/T8) ─────────────────────
  // `connect` is the ONE place the client opens a turn stream — used after a
  // POST (sendTurn), on mount/refresh (resumeStream action → connect), on a
  // stale-tab visibility change, and after a drop (onConnectionLost). It wraps
  // the T6 transport (openTurnStream) and maps the envelope callbacks onto
  // agent-state phase transitions.

  /**
   * Open the GET envelope stream for an already-started turn. Sets an active
   * phase SYNCHRONOUSLY before the first byte (the voice rising-edge in
   * ChatComposer depends on an active phase appearing at turn start), disposes
   * any prior StreamSession (generation bump — preserves the `isCurrent`
   * stale-write guard), then dispatches the batch-apply envelope.
   */
  function connect(agent: string, sessionId: string) {
    // Dispose the previous session (generation bump) before creating a new one,
    // mirroring sendTurn. abortLocalOnly is local-only — it never POSTs
    // /abort, so re-opening the same session id can't cancel it.
    abortLocalOnly(agent);
    const session = streamSessionManager.start(agent);
    // Synchronous active phase (before first byte). Does NOT touch messageSource:
    // the send path's optimistic user echo (sendTurn) must survive, and the
    // resume path lets the replayed envelope establish live mode itself.
    update(agent, {
      connectionPhase: "submitted",
      streamError: null,
      connectionError: null,
      isLlmReconnecting: false,
    });
    recordEventActivity(agent);

    openTurnStream(agent, sessionId, session, {
      onBoundary: (boundaryMessageId) => update(agent, { boundaryMessageId }),
      onEnvelopeApplied: () => update(agent, { connectionPhase: "streaming" }),
      onFinished: () => {
        // Turn is authoritatively over. Do NOT clear live messages here — the
        // id-based live→history handoff (T8) is driven by an effect in
        // ChatThread that watches the refetched sessionMessages and, once the
        // turn's fresh rows are present, sets boundaryMessageId=null + drops the
        // live overlay to history. Until then the frozen overlay stays visible,
        // and the voice falling-edge flush (ChatComposer) reads the last
        // assistant on the render where the phase flips out of an active state.
        update(agent, { connectionPhase: "idle" });
        queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
        queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
      },
      onConnectionLost: () => {
        // Stay "submitted" (still an active phase) and re-open immediately. The
        // visibility/staleness gate below keeps a permanently-dead stream from
        // spinning while the tab is hidden; a genuine mid-turn network drop is
        // resumed by re-opening the same session's envelope.
        update(agent, { connectionPhase: "submitted" });
        connect(agent, sessionId);
      },
    });
  }

  /**
   * Start a new turn: write the optimistic user echo, POST /api/chat via the
   * T6 `startTurn`, then open the GET envelope stream on the session id from the
   * 202 body. This is the single send path used by sendMessage/interruptAndSend.
   */
  async function sendTurn(
    agent: string,
    sessionId: string | null,
    userText: string,
    attachments?: Array<MessageAttachment>,
    userMessageId?: string,
  ) {
    // ── Optimistic user echo ──
    const userParts: MessagePart[] = [];
    if (userText) userParts.push({ type: "text", text: userText });

    const apiAttachments: Array<{
      url: string;
      media_type: string;
      file_name: string;
      mime_type: string;
    }> = [];
    if (attachments && attachments.length > 0) {
      for (const att of attachments) {
        for (const content of att.content) {
          userParts.push({ type: "file", url: content.data, mediaType: content.mimeType });
          apiAttachments.push({
            url: content.data,
            media_type: content.mimeType.startsWith("image/") ? "image" : "document",
            file_name: content.filename ?? att.name,
            mime_type: content.mimeType,
          });
        }
      }
    }
    if (userParts.length === 0) userParts.push({ type: "text", text: "" });

    const userMsg: ChatMessage = {
      id: userMessageId ?? uuid(),
      role: "user",
      parts: userParts,
      createdAt: new Date().toISOString(),
      status: "sending",
    };
    update(agent, {
      messageSource: { mode: "live", messages: [userMsg] },
      streamError: null,
      connectionPhase: "submitted",
      connectionError: null,
      turnLimitMessage: null,
      isLlmReconnecting: false,
      boundaryMessageId: null,
    });
    recordEventActivity(agent);
    saveUiState(agent);

    // ── Build request body ──
    const agentState = store.get().agents[agent];
    const forceNew = agentState?.forceNewSession ?? false;
    const body: Record<string, unknown> = {
      messages: [{ role: "user", content: userText }],
    };
    if (apiAttachments.length > 0) body.attachments = apiAttachments;
    if (sessionId) {
      body.session_id = sessionId;
      // Send leaf_message_id — the tip of the currently viewed branch.
      const rawMsgs = getCachedRawMessages(sessionId);
      if (rawMsgs.length > 0) {
        const branches = store.get().agents[agent]?.selectedBranches ?? {};
        const hasBranching = rawMsgs.some((m) => m.parent_message_id != null);
        if (hasBranching) {
          const activePath = resolveActivePath(rawMsgs, branches);
          if (activePath.length > 0) {
            body.leaf_message_id = activePath[activePath.length - 1].id;
          }
        } else {
          body.leaf_message_id = rawMsgs[rawMsgs.length - 1].id;
        }
      }
    }
    if (userMessageId) body.user_message_id = userMessageId;
    if (forceNew) {
      body.force_new_session = true;
      update(agent, { forceNewSession: false });
    }

    // ── POST → 202 → connect ──
    try {
      const { session_id } = await startTurn(agent, body);
      // Persist last session (was wired via onSessionId → saveLastSession).
      _onSessionId?.(agent, session_id);
      connect(agent, session_id);
    } catch (err) {
      const errMsg = (err instanceof Error ? err.message : "") || "Stream failed";
      // Mark the optimistic user message as failed (SSE-03).
      store.set((draft) => {
        const a = draft.agents[agent];
        if (!a || a.messageSource.mode !== "live") return;
        const msgs = a.messageSource.messages;
        for (let i = msgs.length - 1; i >= 0; i--) {
          if (msgs[i].role === "user" && msgs[i].status === "sending") {
            msgs[i].status = "failed";
            break;
          }
        }
      });
      update(agent, { streamError: errMsg, connectionPhase: "error", connectionError: errMsg });
      saveUiState(agent);
    }
  }

  // ── Callback for saveLastSession (avoids circular import) ─────────────
  let _onSessionId: ((agent: string, sessionId: string) => void) | null = null;

  // ── Visibilitychange recovery ─────────────────────────────────────────
  // Browsers may suspend an SSE socket on a hidden tab; when the tab returns
  // we may have a half-open connection where `reader.read()` is parked and
  // no events are flowing. On visibility=visible we walk every agent that
  // believes it is in an active phase and re-open (via the single `connect`
  // path) when its last observed SSE event is older than VISIBILITY_STALE_MS.
  function handleVisibilityChange() {
    if (_disposed) return;
    if (typeof document === "undefined") return;
    if (document.visibilityState !== "visible") return;
    const now = Date.now();
    const agentsState = store.get().agents as Record<string, AgentState>;
    for (const agent of Object.keys(agentsState)) {
      const st = agentsState[agent];
      if (!st) continue;
      const phase = st.connectionPhase;
      if (phase !== "streaming" && phase !== "submitted") continue;
      const sid = st.activeSessionId;
      if (!sid) continue;
      const last = _lastEventTime.get(agent) ?? 0;
      // If we never recorded activity OR the gap exceeds the threshold, the
      // socket is almost certainly dead — re-open. connect() disposes the prior
      // session itself (generation bump keeps the stale-write guard), never
      // POSTs /abort (tab focus must not cancel the backend turn), and replays
      // the envelope (or an empty finished envelope if the turn already ended).
      if (last !== 0 && now - last < VISIBILITY_STALE_MS) continue;
      try {
        connect(agent, sid);
      } catch (e) {
        if (process.env.NODE_ENV !== "production") {
          console.warn("[streaming-renderer] visibilitychange recovery failed", e);
        }
      }
    }
  }

  let _visibilityListenerAttached = false;
  function attachVisibilityListener() {
    if (_visibilityListenerAttached) return;
    if (typeof document === "undefined") return;
    document.addEventListener("visibilitychange", handleVisibilityChange);
    _visibilityListenerAttached = true;
  }
  attachVisibilityListener();

  // ── MEM-01: Agent cleanup ──────────────────────────────────────────────

  function cleanupAgent(agent: string) {
    // H3: dispose StreamSession to free AbortController + rAF handles
    streamSessionManager.disposeCurrent(agent);
    _lastEventTime.delete(agent);
    // Clean up debounce timers
    clearTimeout(uiStateSaveTimers[agent]);
    delete uiStateSaveTimers[agent];
  }

  /**
   * Full teardown of the renderer: removes the document visibilitychange
   * listener (the one process-wide side effect) and clears every pending
   * debounce timer. Idempotent. After dispose, the visibility handler and the
   * debounced UI-state saver are inert. The live store owns one renderer for
   * the page lifetime, so this is mainly for HMR / tests / any future
   * re-instantiation, none of which should leak a stale listener.
   */
  function dispose() {
    if (_disposed) return;
    _disposed = true;
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    }
    _visibilityListenerAttached = false;
    for (const t of Object.values(uiStateSaveTimers)) clearTimeout(t);
    for (const k of Object.keys(uiStateSaveTimers)) delete uiStateSaveTimers[k];
    _lastEventTime.clear();
  }

  // ── Public API ─────────────────────────────────────────────────────────

  return {
    sendTurn,
    connect,
    abortActiveStream,
    abortLocalOnly,
    cleanupAgent,
    dispose,
    /** Register callback for session ID events (called with agent, sessionId). */
    onSessionId(cb: (agent: string, sessionId: string) => void) {
      _onSessionId = cb;
    },
  };
}

export type StreamingRenderer = ReturnType<typeof createStreamingRenderer>;
