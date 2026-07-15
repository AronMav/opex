// ── streaming-renderer.ts ──────────────────────────────────────────────────
// Factory module encapsulating SSE stream processing, rAF throttling,
// reconnection logic, and per-agent cleanup (MEM-01, PERF-02).

import { scheduleReconnect } from "./stream/stream-reconnect";
import { processSSEStream } from "./stream/stream-processor";
import { startTurn, openTurnStream } from "./stream/chat-stream";
import { MAX_RECONNECT_ATTEMPTS } from "./chat-types";
import { apiPatch, apiPost, assertToken } from "@/lib/api";
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
import type { StreamSession } from "./stream-session";

// ── Store access interface ─────────────────────────────────────────────────
// Typed against the ChatStore shape (type-only import — erased at runtime, so
// there is no circular module dependency with chat-store.ts).

export interface StoreAccess {
  get: () => ChatStore;
  set: (fn: (draft: ChatStore) => void) => void;
}
// ── Reconnect constants (SSE-02) ─────────────────────────────────────────────
const RECONNECT_DELAY_BASE_MS = 1000;

// ── Factory ────────────────────────────────────────────────────────────────

export function createStreamingRenderer(store: StoreAccess) {
  // ── CLN-02: Encapsulated non-serializable state ──────────────────────────
  // setTimeout handles are not plain objects -- Immer cannot proxy or freeze them.
  // They live in private Maps inside the factory closure.
  // AbortController now lives inside StreamSession.signal (Task 3.6).

  const _reconnectTimers = new Map<string, ReturnType<typeof setTimeout> | null>();
  // Set by dispose(): stops the visibilitychange handler and blocks any new
  // debounce timers from being scheduled after teardown.
  let _disposed = false;

  function getReconnectTimer(agent: string): ReturnType<typeof setTimeout> | null {
    return _reconnectTimers.get(agent) ?? null;
  }
  function setReconnectTimer(agent: string, timer: ReturnType<typeof setTimeout> | null): void {
    _reconnectTimers.set(agent, timer);
  }

  // Fix #6: Track the last time we saw any SSE traffic per agent. Browsers
  // (especially Chrome) throttle / suspend SSE sockets on hidden tabs;
  // when the tab returns we may have a half-open connection where the
  // reader.read() is parked and no events are coming. The visibilitychange
  // listener below forces a reconnect for any agent whose stream has been
  // silent for more than VISIBILITY_STALE_MS while in an active phase.
  const _lastEventTime = new Map<string, number>();
  const VISIBILITY_STALE_MS = 30_000;

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

  /**
   * Returns true when RQ cache says the session is no longer running. The
   * backend can close SSE without sending a `finish` event on some exit paths
   * (subagent timeout, dropped tool result, finalize bypass) and still mark
   * the session row as `done` in DB. The frontend would otherwise loop trying
   * to resume — checking the cached status short-circuits that loop.
   */
  function isSessionFinishedInCache(agent: string, sessionId: string): boolean {
    const cached = queryClient.getQueryData<{ sessions: { id: string; run_status?: string }[] }>(qk.sessions(agent));
    const status = cached?.sessions?.find((s) => s.id === sessionId)?.run_status;
    return !!status && status !== "running";
  }

  /** Switch to history mode for a session that the backend has finalized. */
  function settleAsFinished(session: StreamSession, agent: string, sessionId: string) {
    session.write({
      connectionPhase: "idle",
      messageSource: { mode: "history", sessionId },
      isLlmReconnecting: false,
      reconnectAttempt: 0,
      streamError: null,
    });
    queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
    queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
  }

  // ── Stream lifecycle ────────────────────────────────────────────────────

  /**
   * Resume an active backend stream after page reload.
   * Connects to GET /api/chat/{sessionId}/stream and processes replay + live events.
   */
  function resumeStream(agent: string, sessionId: string, reconnectAttempt = 0) {
    // Don't resume if already streaming (but allow reconnect path even in "reconnecting" phase)
    const st = store.get().agents[agent];
    if (st && st.connectionPhase === "streaming") return;

    // Clear any existing reconnect timer before starting a new stream
    const existingTimer = getReconnectTimer(agent);
    if (existingTimer) {
      clearTimeout(existingTimer);
      setReconnectTimer(agent, null);
    }
    // Local-only cleanup: DO NOT POST /abort here. The previous stream on
    // the same session id may have already ended, and if we POST /abort
    // during startup, the backend cancels the stream we are about to start
    // (same session id → same cancel token).

    // Local-only cleanup of the previous fetch controller. Removed in Task 3.6
    // together with the legacy _abortControllers / _reconnectTimers maps.
    abortLocalOnly(agent);

    // Create a new StreamSession after abortLocalOnly's generation bump.
    // streamSessionManager.start() disposes the previous session (bumping
    // generation once) and creates a new session whose .generation is the
    // current store value — used as the authoritative generation reference
    // inside processSSEStream.
    const session = streamSessionManager.start(agent);

    // Architecture C: live messages = overlay only (current streaming message).
    // History comes from React Query. No seed needed.
    //
    // Use "submitted" (not "streaming") to mark the request as in-flight
    // before any SSE bytes have arrived. processSSEStream upgrades the phase
    // to "streaming" on the first data byte; if the server returns 204 (no
    // events) we transition submitted → idle directly. Setting "streaming"
    // here would lie about the wire state and break downstream consumers
    // (e.g. the bootstrap auto-resume effect in ChatThread) that distinguish
    // "request sent, waiting for first byte" from "actively streaming".
    update(agent, {
      streamError: null,
      connectionPhase: "submitted",
      connectionError: null,
      messageSource: { mode: "live" as const, messages: [] },
    });

    // Seed _lastEventTime so visibilitychange recovery does not force-reconnect
    // a freshly-submitted stream during the first-event window.
    recordEventActivity(agent);

    const token = assertToken();

    // Pass Last-Event-ID so backend replays only events newer than the last
    // one the client processed (Phase 3 offset tracking). Carries across the
    // session disposal-and-recreate that resumeStream does, because the
    // *previous* session's id was preserved into agent state on prior runs;
    // we read that via store. New stream → no header, full replay.
    const lastEventHeader: Record<string, string> = {};
    const prevId = store.get().agents[agent]?.lastEventId;
    if (typeof prevId === "number" && prevId > 0) {
      lastEventHeader["Last-Event-ID"] = String(prevId);
    }

    fetch(`/api/chat/${sessionId}/stream?agent=${encodeURIComponent(agent)}`, {
      method: "GET",
      headers: { Authorization: `Bearer ${token}`, ...lastEventHeader },
      signal: session.signal,
    })
      .then((resp) => {
        if (resp.status === 204) {
          // No active stream -- engine already finished. Switch to history and refetch.
          // Guard: if abort fired or a newer stream started during the
          // fetch, discard this response. Without the guard a late 204
          // would force messageSource back to the resumed session
          // after the user had already navigated away.
          if (!session.isCurrent || session.signal.aborted) {
            return;
          }
          session.write({ connectionPhase: "idle", messageSource: { mode: "history", sessionId } });
          queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
          queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
          return;
        }
        if (resp.status === 401) {
          import("@/lib/api").then(({ handleUnauthorized }) => handleUnauthorized());
          return;
        }
        if (!resp.ok) {
          return resp.text().then((t) => { throw new Error(t || `HTTP ${resp.status}`); });
        }
        return processSSEStream(session, resp.body!, {
          sessionId,
          reconnectAttempt,
          callbacks: {
            onSessionId: (sid) => { _onSessionId?.(agent, sid); },
            onReconnectNeeded: (sid, attempt) => {
              if (isSessionFinishedInCache(agent, sid)) {
                settleAsFinished(session, agent, sid);
                return;
              }
              scheduleReconnect(session, sid, attempt, {
                resume: (nextAttempt) => resumeStream(agent, sid, nextAttempt),
                maxAttempts: MAX_RECONNECT_ATTEMPTS,
                baseDelayMs: RECONNECT_DELAY_BASE_MS,
                setTimer: (handle) => setReconnectTimer(agent, handle),
                onMaxAttemptsReached: () => settleAsFinished(session, agent, sid),
              });
            },
            getAgentState: (a) => store.get().agents[a],
            updateSessionParticipants: (sid, participants) => store.get().updateSessionParticipants(sid, participants),
            onStreamDone: () => saveUiState(agent),
            onEventActivity: () => recordEventActivity(agent),
          },
        });
      })
      .catch((err) => {
        if (err.name === "AbortError") return;
        // Guard: if a newer stream started, don't schedule reconnect for the old one
        if (!session.isCurrent) return;
        // Network error during reconnect -- schedule next retry
        scheduleReconnect(session, sessionId, reconnectAttempt, {
          resume: (nextAttempt) => resumeStream(agent, sessionId, nextAttempt),
          maxAttempts: MAX_RECONNECT_ATTEMPTS,
          baseDelayMs: RECONNECT_DELAY_BASE_MS,
          setTimer: (handle) => setReconnectTimer(agent, handle),
          onMaxAttemptsReached: () => {
            session.write({
              connectionPhase: "idle",
              messageSource: { mode: "history", sessionId },
              isLlmReconnecting: false,
              reconnectAttempt: 0,
              streamError: null,
            });
            queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
            queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
          },
        });
      });
  }

  /** Internal: local abort only (no backend notification). Used by
   * startStream to clean up lingering fetch controllers before launching
   * a new stream on the same agent. Calling /abort here would race with
   * the new stream's registration on the same session id and cancel it
   * prematurely.
   *
   * Fix #1 / #2: the pending reconnect timer is cleared FIRST so a
   * setTimeout that just elapsed cannot race the abort and re-enter
   * resumeStream against the (now-stale) generation. cleanupAgent()
   * delegates to this same path during agent switch, so per-agent
   * reconnect timers are also dropped when the user switches agent.
   */
  function abortLocalOnly(agent: string) {
    const timer = getReconnectTimer(agent);
    if (timer) { clearTimeout(timer); setReconnectTimer(agent, null); }
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
        a.reconnectAttempt = 0;
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

  // ── SSE stream handler ──────────────────────────────────────────────────
  // Reconnect policy is in stream/stream-reconnect.ts (SSE-02).

  function startStream(agent: string, sessionId: string | null, messages: ChatMessage[], userText: string, attachments?: Array<MessageAttachment>, userMessageId?: string) {
    // Local-only cleanup for the same reason documented in resumeStream.
    abortLocalOnly(agent);

    // Create a new StreamSession after abortLocalOnly's generation bump.
    // streamSessionManager.start() disposes the previous session (bumping
    // generation once) and creates a new session whose .generation is the
    // current store value — used as the authoritative generation reference
    // inside processSSEStream.
    const session = streamSessionManager.start(agent);

    // Reset Last-Event-ID — backend's seq counter is per-session and starts
    // at 1 for any new session_id. Without this, a leftover id from the
    // previous session would tell the backend to skip every event of the
    // new session (seq <= stale_last_id) and the UI would freeze empty.
    session.write({ lastEventId: null });

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
          userParts.push({
            type: "file",
            url: content.data,
            mediaType: content.mimeType,
          });

          apiAttachments.push({
            url: content.data,
            media_type: content.mimeType.startsWith("image/") ? "image" : "document",
            file_name: content.filename ?? att.name,
            mime_type: content.mimeType,
          });
        }
      }
    }

    if (userParts.length === 0) {
      userParts.push({ type: "text", text: "" });
    }

    // Build user message — use the pre-allocated UUID so the optimistic message
    // ID matches the DB row after bootstrap saves it via save_message_ex_with_id.
    const userMsg: ChatMessage = {
      id: userMessageId ?? uuid(),
      role: "user",
      parts: userParts,
      createdAt: new Date().toISOString(),
      status: "sending",
    };
    // Architecture C: live = overlay only. History provides past messages.
    // Overlay contains just the optimistic user message (until history picks it up).
    update(agent, {
      messageSource: { mode: "live", messages: [userMsg] },
      streamError: null,
      connectionPhase: "submitted",
      connectionError: null,
      turnLimitMessage: null,
      reconnectAttempt: 0,
      isLlmReconnecting: false,
    });
    // Seed _lastEventTime so visibilitychange recovery does not force-reconnect
    // a freshly-submitted stream during the first-event window.
    recordEventActivity(agent);
    saveUiState(agent);

    // Build request body -- backend only uses the last user message + session_id
    const agentState = store.get().agents[agent];
    const forceNew = agentState?.forceNewSession ?? false;
    const body: Record<string, unknown> = {
      agent,
      messages: [{ role: "user", content: userText }],
    };
    if (apiAttachments.length > 0) {
      body.attachments = apiAttachments;
    }
    if (sessionId) {
      body.session_id = sessionId;
      // Send leaf_message_id — the tip of the currently viewed branch.
      // Use resolveActivePath to find the correct leaf (not the absolute last message,
      // which could be on a different branch).
      const rawMsgs = getCachedRawMessages(sessionId);
      if (rawMsgs.length > 0) {
        const agentSt = store.get().agents[agent];
        const branches = agentSt?.selectedBranches ?? {};
        const hasBranching = rawMsgs.some(m => m.parent_message_id != null);
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
    if (userMessageId) {
      body.user_message_id = userMessageId;
    }
    if (forceNew) {
      body.force_new_session = true;
      update(agent, { forceNewSession: false });
    }

    const token = assertToken();

    fetch("/api/chat", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${token}`,
      },
      body: JSON.stringify(body),
      signal: session.signal,
    })
      .then((resp) => {
        if (resp.status === 401) {
          import("@/lib/api").then(({ handleUnauthorized }) => handleUnauthorized());
          return;
        }
        if (!resp.ok) {
          return resp.text().then((t) => {
            throw new Error(t || `HTTP ${resp.status}`);
          });
        }
        return processSSEStream(session, resp.body!, {
          sessionId: null,
          reconnectAttempt: 0,
          callbacks: {
            onSessionId: (sid) => { _onSessionId?.(agent, sid); },
            onReconnectNeeded: (sid, attempt) => {
              if (isSessionFinishedInCache(agent, sid)) {
                settleAsFinished(session, agent, sid);
                return;
              }
              scheduleReconnect(session, sid, attempt, {
                resume: (nextAttempt) => resumeStream(agent, sid, nextAttempt),
                maxAttempts: MAX_RECONNECT_ATTEMPTS,
                baseDelayMs: RECONNECT_DELAY_BASE_MS,
                setTimer: (handle) => setReconnectTimer(agent, handle),
                onMaxAttemptsReached: () => settleAsFinished(session, agent, sid),
              });
            },
            getAgentState: (a) => store.get().agents[a],
            updateSessionParticipants: (sid, participants) => store.get().updateSessionParticipants(sid, participants),
            onStreamDone: () => saveUiState(agent),
            onEventActivity: () => recordEventActivity(agent),
          },
        });
      })
      .catch((err) => {
        if (err.name === "AbortError") return;
        const errMsg = err.message || "Stream failed";
        // SSE-03: Mark the optimistic user message as failed so the UI shows an error indicator.
        session.writeDraft((agentDraft: AgentState) => {
          if (agentDraft.messageSource.mode !== "live") return;
          const msgs = agentDraft.messageSource.messages;
          for (let i = msgs.length - 1; i >= 0; i--) {
            if (msgs[i].role === "user" && msgs[i].status === "sending") {
              msgs[i].status = "failed";
              break;
            }
          }
        });
        update(agent, {
          streamError: errMsg,
          connectionPhase: "error",
          connectionError: errMsg,
        });
        saveUiState(agent);
      });
  }

  // ── Server-authoritative single connect path (T7) ────────────────────────
  // `connect` is the ONE place the client opens a turn stream — used after a
  // POST (sendTurn), on mount/refresh (resumeStream action), and after a drop
  // (onConnectionLost). It wraps the T6 transport (openTurnStream) and maps the
  // envelope callbacks onto agent-state phase transitions.

  /**
   * Open the GET envelope stream for an already-started turn. Sets an active
   * phase SYNCHRONOUSLY before the first byte (the voice rising-edge in
   * ChatComposer depends on an active phase appearing at turn start), disposes
   * any prior StreamSession (generation bump — preserves the `isCurrent`
   * stale-write guard), then dispatches the batch-apply envelope.
   */
  function connect(agent: string, sessionId: string) {
    // Dispose the previous session (generation bump) before creating a new one,
    // mirroring startStream/resumeStream. abortLocalOnly is local-only — it
    // never POSTs /abort, so re-opening the same session id can't cancel it.
    abortLocalOnly(agent);
    const session = streamSessionManager.start(agent);
    // New stream → backend seq restarts; drop any stale Last-Event-ID.
    session.write({ lastEventId: null });
    // Synchronous active phase (before first byte). Does NOT touch messageSource:
    // the send path's optimistic user echo (sendTurn) must survive, and the
    // resume path lets the replayed envelope establish live mode itself.
    update(agent, {
      connectionPhase: "submitted",
      streamError: null,
      connectionError: null,
      reconnectAttempt: 0,
      isLlmReconnecting: false,
    });
    recordEventActivity(agent);

    openTurnStream(agent, sessionId, session, {
      onBoundary: (boundaryMessageId) => update(agent, { boundaryMessageId }),
      onEnvelopeApplied: () => update(agent, { connectionPhase: "streaming" }),
      onFinished: () => {
        // Turn is authoritatively over. Do NOT clear live messages here — the
        // id-based live→history handoff lands in T8, and the voice falling-edge
        // flush (ChatComposer) reads the last assistant on the render where the
        // phase flips out of an active state. stream-processor's own
        // finishing→history dance keeps the overlay frozen until RQ refetch.
        update(agent, { connectionPhase: "idle" });
        queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
        queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
      },
      onConnectionLost: () => {
        // Stay "submitted" (still an active phase) and re-open immediately. T8
        // will gate this with staleness / visibility so a permanently dead
        // stream can't spin.
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
    // ── Optimistic user echo (formerly startStream:350-401) ──
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
      reconnectAttempt: 0,
      isLlmReconnecting: false,
      boundaryMessageId: null,
    });
    recordEventActivity(agent);
    saveUiState(agent);

    // ── Build request body (formerly startStream:407-443) ──
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

  // ── Fix #6: visibilitychange recovery ─────────────────────────────────
  // Browsers may suspend an SSE socket on a hidden tab; when the tab returns
  // we may have a half-open connection where `reader.read()` is parked and
  // no events are flowing. On visibility=visible we walk every agent that
  // believes it is in an active phase and force a soft reconnect when its
  // last observed SSE event is older than VISIBILITY_STALE_MS.
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
      // If we never recorded activity OR the gap exceeds the threshold,
      // the socket is almost certainly dead — abort and resume.
      if (last !== 0 && now - last < VISIBILITY_STALE_MS) continue;
      try {
        // Local-only (no /abort POST): tab focus shouldn't tell the
        // backend to cancel. resumeStream re-attaches to the live stream
        // (or 204 + history if it has already finished).
        abortLocalOnly(agent);
        resumeStream(agent, sid);
      } catch (e) {
        if (process.env.NODE_ENV !== "production") {
          // eslint-disable-next-line no-console
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
    const timer = _reconnectTimers.get(agent);
    if (timer) clearTimeout(timer);
    _reconnectTimers.delete(agent);
    _lastEventTime.delete(agent);
    // Clean up debounce timers
    clearTimeout(uiStateSaveTimers[agent]);
    delete uiStateSaveTimers[agent];
  }

  /**
   * Full teardown of the renderer: removes the document visibilitychange
   * listener (the one process-wide side effect) and clears every pending
   * debounce/reconnect timer. Idempotent. After dispose, the visibility
   * handler and the debounced UI-state saver are inert. The live store owns
   * one renderer for the page lifetime, so this is mainly for HMR / tests /
   * any future re-instantiation, none of which should leak a stale listener.
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
    for (const t of _reconnectTimers.values()) if (t) clearTimeout(t);
    _reconnectTimers.clear();
    _lastEventTime.clear();
  }

  // ── Public API ─────────────────────────────────────────────────────────

  return {
    startStream,
    sendTurn,
    connect,
    resumeStream,
    abortActiveStream,
    abortLocalOnly,
    cleanupAgent,
    dispose,
    getReconnectTimer,
    setReconnectTimer,
    /** Register callback for session ID events (called with agent, sessionId). */
    onSessionId(cb: (agent: string, sessionId: string) => void) {
      _onSessionId = cb;
    },
  };
}

export type StreamingRenderer = ReturnType<typeof createStreamingRenderer>;
