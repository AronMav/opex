// ── streaming-renderer.ts ──────────────────────────────────────────────────
// Factory module encapsulating SSE stream processing, rAF throttling,
// reconnection logic, and per-agent cleanup (MEM-01, PERF-02).

import { scheduleReconnect } from "./stream/stream-reconnect";
import { processSSEStream } from "./stream/stream-processor";
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
  AgentState,
} from "./chat-types";
import { getCachedRawMessages, resolveActivePath } from "./chat-history";
import { streamSessionManager } from "./stream-session";
import type { StreamSession } from "./stream-session";

// ── Store access interface ─────────────────────────────────────────────────
// Uses `any` for store shape to avoid circular dependency with ChatStore.

interface StoreAccess {
  get: () => any;
  set: (fn: (draft: any) => void) => void;
}

// ── Reconnect constants (SSE-02) ─────────────────────────────────────────────
const MAX_RECONNECT_ATTEMPTS = 3;
const RECONNECT_DELAY_BASE_MS = 1000;

// ── Factory ────────────────────────────────────────────────────────────────

export function createStreamingRenderer(store: StoreAccess) {
  // ── CLN-02: Encapsulated non-serializable state ──────────────────────────
  // setTimeout handles are not plain objects -- Immer cannot proxy or freeze them.
  // They live in private Maps inside the factory closure.
  // AbortController now lives inside StreamSession.signal (Task 3.6).

  const _reconnectTimers = new Map<string, ReturnType<typeof setTimeout> | null>();

  function getReconnectTimer(agent: string): ReturnType<typeof setTimeout> | null {
    return _reconnectTimers.get(agent) ?? null;
  }
  function setReconnectTimer(agent: string, timer: ReturnType<typeof setTimeout> | null): void {
    _reconnectTimers.set(agent, timer);
  }

  // ── Internal helpers ────────────────────────────────────────────────────

  function ensure(agent: string): AgentState {
    const s = store.get().agents[agent];
    if (s) return s;
    const fresh = emptyAgentState();
    store.set((draft: any) => { draft.agents[agent] = fresh; });
    return fresh;
  }

  function update(agent: string, patch: Partial<AgentState>) {
    store.set((draft: any) => {
      if (!draft.agents[agent]) draft.agents[agent] = emptyAgentState();
      Object.assign(draft.agents[agent], patch);
    });
  }

  // ── Debounced UI state persistence to server ──────────────────────────────
  const uiStateSaveTimers: Record<string, ReturnType<typeof setTimeout>> = {};
  function saveUiState(agent: string) {
    clearTimeout(uiStateSaveTimers[agent]);
    uiStateSaveTimers[agent] = setTimeout(() => {
      const st = store.get().agents[agent];
      if (!st?.activeSessionId) return;
      apiPatch(`/api/sessions/${st.activeSessionId}`, {
        ui_state: { connectionPhase: st.connectionPhase },
      }).catch((e: unknown) => { console.warn("[chat] save failed:", e); });
    }, 500);
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
    update(agent, {
      streamError: null,
      connectionPhase: "streaming",
      connectionError: null,
      messageSource: { mode: "live" as const, messages: [] },
    });

    const token = assertToken();

    fetch(`/api/chat/${sessionId}/stream`, {
      method: "GET",
      headers: { Authorization: `Bearer ${token}` },
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
              scheduleReconnect(session, sid, attempt, {
                resume: (nextAttempt) => resumeStream(agent, sid, nextAttempt),
                maxAttempts: MAX_RECONNECT_ATTEMPTS,
                baseDelayMs: RECONNECT_DELAY_BASE_MS,
                setTimer: (handle) => setReconnectTimer(agent, handle),
              });
            },
            getAgentState: (a) => store.get().agents[a],
            updateSessionParticipants: (sid, participants) => store.get().updateSessionParticipants(sid, participants),
            onStreamDone: () => saveUiState(agent),
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
        });
      });
  }

  /** Internal: local abort only (no backend notification). Used by
   * startStream to clean up lingering fetch controllers before launching
   * a new stream on the same agent. Calling /abort here would race with
   * the new stream's registration on the same session id and cancel it
   * prematurely.
   */
  function abortLocalOnly(agent: string) {
    const timer = getReconnectTimer(agent);
    if (timer) { clearTimeout(timer); setReconnectTimer(agent, null); }
    streamSessionManager.disposeCurrent(agent);
    // `dispose()` lands the final `connectionPhase: "idle"` write and
    // bumps `streamGeneration` atomically. No direct store mutation
    // here — the grep guard (Task 3.8) enforces that stream-state
    // fields are never touched outside StreamSession.
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
      apiPost(`/api/chat/${sid}/abort`).catch(() => {
        // Backend may not have an active stream (already done / not started).
        // Local abort below still cleans up UI state.
      });
    }
    abortLocalOnly(agent);
  }

  // ── SSE stream handler ──────────────────────────────────────────────────
  // Reconnect policy is in stream/stream-reconnect.ts (SSE-02).

  function startStream(agent: string, sessionId: string | null, messages: ChatMessage[], userText: string, attachments?: Array<any>, userMessageId?: string) {
    // Local-only cleanup for the same reason documented in resumeStream.
    abortLocalOnly(agent);

    // Create a new StreamSession after abortLocalOnly's generation bump.
    // streamSessionManager.start() disposes the previous session (bumping
    // generation once) and creates a new session whose .generation is the
    // current store value — used as the authoritative generation reference
    // inside processSSEStream.
    const session = streamSessionManager.start(agent);

    const userParts: MessagePart[] = [];
    if (userText) userParts.push({ type: "text", text: userText });

    const apiAttachments: any[] = [];
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

    // Build user message -- optimistic status: "sending" until data-session-id confirms receipt
    const userMsg: ChatMessage = {
      id: uuid(),
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
              scheduleReconnect(session, sid, attempt, {
                resume: (nextAttempt) => resumeStream(agent, sid, nextAttempt),
                maxAttempts: MAX_RECONNECT_ATTEMPTS,
                baseDelayMs: RECONNECT_DELAY_BASE_MS,
                setTimer: (handle) => setReconnectTimer(agent, handle),
              });
            },
            getAgentState: (a) => store.get().agents[a],
            updateSessionParticipants: (sid, participants) => store.get().updateSessionParticipants(sid, participants),
            onStreamDone: () => saveUiState(agent),
          },
        });
      })
      .catch((err) => {
        if (err.name === "AbortError") return;
        const errMsg = err.message || "Stream failed";
        // SSE-03: Mark the optimistic user message as failed so the UI shows an error indicator.
        session.writeDraft((agentDraft: AgentState) => {
          if (agentDraft.messageSource.mode !== "live") return;
          const msgs = (agentDraft.messageSource as any).messages as ChatMessage[];
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

  // ── Callback for saveLastSession (avoids circular import) ─────────────
  let _onSessionId: ((agent: string, sessionId: string) => void) | null = null;

  // ── MEM-01: Agent cleanup ──────────────────────────────────────────────

  function cleanupAgent(agent: string) {
    // H3: dispose StreamSession to free AbortController + rAF handles
    streamSessionManager.disposeCurrent(agent);
    const timer = _reconnectTimers.get(agent);
    if (timer) clearTimeout(timer);
    _reconnectTimers.delete(agent);
    // Clean up debounce timers
    clearTimeout(uiStateSaveTimers[agent]);
    delete uiStateSaveTimers[agent];
  }

  // ── Public API ─────────────────────────────────────────────────────────

  return {
    startStream,
    resumeStream,
    abortActiveStream,
    abortLocalOnly,
    cleanupAgent,
    getReconnectTimer,
    setReconnectTimer,
    /** Register callback for session ID events (called with agent, sessionId). */
    onSessionId(cb: (agent: string, sessionId: string) => void) {
      _onSessionId = cb;
    },
  };
}

export type StreamingRenderer = ReturnType<typeof createStreamingRenderer>;
