// ── streaming-renderer.ts ──────────────────────────────────────────────────
// Factory module encapsulating SSE stream processing, rAF throttling, the
// single connect path (POST 202 then GET envelope) + envelope streaming, and
// per-agent cleanup (MEM-01, PERF-02).

import { startTurn, openTurnStream } from "./stream/chat-stream";
import { apiPatch, apiPost } from "@/lib/api";

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
  //
  // `recordEventActivity` is called once synchronously at `connect()` time AND
  // on every subsequent parsed SSE event (wired through `onEventActivity` on
  // the `openTurnStream` callbacks, sourced from `processSSEStream` in
  // stream-processor.ts). So `_lastEventTime` again means what its name says —
  // "time of last event" — not merely "time since connect".
  const _lastEventTime = new Map<string, number>();
  // T8: tightened from 30_000 → 15_000. The single connect path returns an
  // empty envelope cheaply when there is no in-flight turn, so a shorter
  // staleness window recovers a suspended socket faster with negligible cost.
  const VISIBILITY_STALE_MS = 15_000;

  // ── Fix I: bounded reconnect (backoff + cap) ─────────────────────────────
  // `onConnectionLost` previously re-`connect`ed immediately with no delay,
  // backoff or cap. Against a persistently failing GET (server down, deleted
  // session → 404/500) that tight-loops forever, hammering the server and
  // pinning a "thinking" spinner. We now schedule the retry with exponential
  // backoff and give up after RECONNECT_MAX_RETRIES, surfacing a visible error.
  const RECONNECT_BASE_DELAY_MS = 500;
  const RECONNECT_MAX_DELAY_MS = 15_000;
  const RECONNECT_MAX_RETRIES = 6;
  const _reconnectAttempts = new Map<string, number>();
  const _reconnectTimers = new Map<string, ReturnType<typeof setTimeout>>();

  function clearReconnectTimer(agent: string): void {
    const t = _reconnectTimers.get(agent);
    if (t !== undefined) {
      clearTimeout(t);
      _reconnectTimers.delete(agent);
    }
  }

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
    // Fix I: cancel any pending backoff reconnect. (Attempts counter is NOT
    // reset here — a retry re-enters via connect(), which clears it on the
    // non-retry path; a user Stop / nav is followed by a fresh non-retry
    // connect that resets it too.)
    clearReconnectTimer(agent);
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
  function connect(agent: string, sessionId: string, isRetry = false) {
    // Dispose the previous session (generation bump) before creating a new one,
    // mirroring sendTurn. abortLocalOnly is local-only — it never POSTs
    // /abort, so re-opening the same session id can't cancel it.
    abortLocalOnly(agent);
    // Fix I: a fresh (non-retry) connect — new send, resume, or visibility
    // recovery — starts with a clean reconnect budget. Retries preserve the
    // running count so the cap is actually reached under persistent failure.
    if (!isRetry) _reconnectAttempts.delete(agent);
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
      onEnvelopeApplied: () => {
        // Fix I: the envelope committed — the connection is healthy, so a
        // later drop gets a fresh reconnect budget (the cap targets a
        // stream that NEVER connects, not intermittent mid-turn drops).
        _reconnectAttempts.delete(agent);
        update(agent, { connectionPhase: "streaming" });
      },
      onFinished: () => {
        _reconnectAttempts.delete(agent);
        clearReconnectTimer(agent);
        // Turn is over. Query invalidation + refetch + history settle are
        // owned EXCLUSIVELY by stream-processor's post-finally; here we only
        // idle the phase and reset the reconnect budget.
        update(agent, { connectionPhase: "idle" });
      },
      onConnectionLost: () => scheduleReconnect(agent, sessionId),
      onEventActivity: () => recordEventActivity(agent),
    });
  }

  /**
   * Fix I: schedule a bounded, backed-off reconnect after a drop without a
   * terminal signal. Exponential backoff (`RECONNECT_BASE_DELAY_MS * 2^n`,
   * clamped to `RECONNECT_MAX_DELAY_MS`) with a `RECONNECT_MAX_RETRIES` cap.
   * On the cap we STOP retrying and surface a visible error state instead of
   * tight-looping against a persistently failing GET. Uses `setTimeout` (not
   * rAF) so tests can drive it with fake timers.
   */
  function scheduleReconnect(agent: string, sessionId: string) {
    const attempts = (_reconnectAttempts.get(agent) ?? 0) + 1;
    _reconnectAttempts.set(agent, attempts);

    if (attempts > RECONNECT_MAX_RETRIES) {
      // Give up — the stream is persistently unreachable (server down, deleted
      // session → 4xx/5xx). Surface a real error rather than spin forever.
      _reconnectAttempts.delete(agent);
      clearReconnectTimer(agent);
      update(agent, {
        connectionPhase: "error",
        isLlmReconnecting: false,
        connectionError: "reconnect-failed",
        streamError: "Соединение потеряно. Не удалось переподключиться.",
      });
      return;
    }

    // Still within budget — show the reconnecting indicator during the wait.
    update(agent, { connectionPhase: "submitted", isLlmReconnecting: true });
    const delay = Math.min(
      RECONNECT_BASE_DELAY_MS * 2 ** (attempts - 1),
      RECONNECT_MAX_DELAY_MS,
    );
    clearReconnectTimer(agent);
    const timer = setTimeout(() => {
      _reconnectTimers.delete(agent);
      if (_disposed) return;
      connect(agent, sessionId, true);
    }, delay);
    _reconnectTimers.set(agent, timer);
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
      const rawMsgs = getCachedRawMessages(sessionId, agent);
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
    // Fix I: drop any pending reconnect + its attempt counter for this agent.
    clearReconnectTimer(agent);
    _reconnectAttempts.delete(agent);
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
    // Fix I: cancel every pending reconnect timer and clear attempt counters.
    for (const t of _reconnectTimers.values()) clearTimeout(t);
    _reconnectTimers.clear();
    _reconnectAttempts.clear();
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
