// ── streaming-renderer.ts ──────────────────────────────────────────────────
// Factory module encapsulating SSE stream processing, rAF throttling, the
// single connect path (POST 202 then GET envelope) + envelope streaming, and
// per-agent cleanup (MEM-01, PERF-02).

import { startTurn, openTurnStream } from "./stream/chat-stream";
import { apiPatch, apiPost } from "@/lib/api";

import { uuid } from "./chat-types";
import type {
  ChatMessage,
  MessagePart,
  MessageAttachment,
  AgentState,
  ChatStore,
} from "./chat-types";
import { getCachedRawMessages, resolveActivePath } from "./chat-history";
import { streamSessionManager } from "./stream-session";
import { makeUpdate } from "./chat/actions/_shared";
import { saveLastSession } from "./chat-persistence";

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
  // C2 fix: focused-tab activity watchdog. `reader.read()` may park forever
  // on a half-open TCP connection (LB idle timeout, NAT rebinding, WiFi flap
  // before TCP keepalive surfaces RST) — `onConnectionLost` does NOT fire and
  // the visibility handler is a no-op while the tab stays focused. The
  // watchdog polls `_lastEventTime` every interval and reattaches any agent
  // whose stream has been silent for longer than the threshold while it
  // remains in an active phase.
  const ACTIVITY_WATCHDOG_INTERVAL_MS = 5_000;
  const ACTIVITY_WATCHDOG_STALE_MS = 30_000;

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
  // NOTE: `makeUpdate` is resolved lazily inside the function body (not
  // `const update = makeUpdate(store.set)` at construction time) — this module
  // sits in a real import cycle (streaming-renderer → stream/chat-stream →
  // chat-store → streaming-renderer), and eagerly dereferencing an import
  // binding while that cycle is still resolving throws a TDZ ReferenceError.
  // Deferring the reference into the function body (only touched when
  // `update()` is actually called, well after module load settles) avoids it.

  function update(agent: string, patch: Partial<AgentState>): void {
    makeUpdate(store.set)(agent, patch);
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
   * NOTE: cleanupAgent() (agent switch / unmount teardown, below) does NOT
   * delegate here — it calls streamSessionManager.disposeCurrent() directly,
   * since MEM-01 cleanup only needs to free the AbortController/rAF handles
   * and clear this module's per-agent timers, not settle message statuses.
   *
   * `settleMessages` (default true) controls the streaming-status sweep at
   * the bottom. Intentional teardown — user Stop (abortActiveStream),
   * session/agent switch (navigation actions), regenerate (stream-control) —
   * keeps the default: the turn is being abandoned, so its caret must stop.
   * The reconnect-CONTINUATION path (connect()'s pre-open cleanup — same
   * turn, new envelope) passes false: sweeping there would mark the
   * still-streaming message "complete", and the no-downgrade guards (sync's
   * `status !== "complete"` check and commit()'s guard) make that permanent —
   * after any transient drop or visibility-stale recovery the text would
   * keep appending but the caret would never return.
   */
  function abortLocalOnly(agent: string, opts?: { settleMessages?: boolean }) {
    const settleMessages = opts?.settleMessages ?? true;
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
    // Settle streaming message statuses (intentional teardown only — see the
    // `settleMessages` doc above). On abort the stream-processor's
    // finally-block commit is a no-op (disposeCurrent above bumped the
    // generation → isCurrent false) and its post-finally handoff is gated on
    // `!signal.aborted` — so nothing else stops the inline caret. Sweep every
    // live/finishing message still marked "streaming" to "complete".
    // NOT gated on an active connectionPhase: dispose() lands its own
    // `connectionPhase: "idle"` write before we read the phase above,
    // so gating the sweep on an active phase would skip the common Stop flow.
    // Runs AFTER the generation bump — must be a direct store write, not a
    // session.commit (which would silently drop as stale).
    if (settleMessages) {
      store.set((draft) => {
        const a = draft.agents[agent];
        if (!a) return;
        const src = a.messageSource;
        const msgs = src.mode === "live" || src.mode === "finishing" ? src.messages : [];
        for (const m of msgs) {
          if (m.status === "streaming") m.status = "complete";
        }
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
  function abortActiveStream(agent: string): Promise<void> {
    const sid = store.get().agents[agent]?.activeSessionId;
    // C4 fix: return the POST promise so callers that need to wait for the
    // backend to ack the abort (notably `interruptAndSend`) can do so before
    // starting a new turn on the same session id. The previous fire-and-forget
    // shape let the POST race past the new POST /api/chat and silently cancel
    // the fresh turn.
    const post = sid
      ? apiPost(`/api/chat/${sid}/abort?agent=${encodeURIComponent(agent)}`)
          .then(() => undefined)
          .catch(() => {
            // Backend may not have an active stream (already done / not started).
            // Local abort below still cleans up UI state.
          })
      : Promise.resolve();
    abortLocalOnly(agent);
    return post;
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
    // settleMessages: false — this cleanup is a reconnect CONTINUATION of the
    // same turn (drop-recovery, visibility-stale, resume, post-POST open),
    // not a teardown: the still-streaming message must keep status
    // "streaming" until the envelope settles it, or the no-downgrade guards
    // would freeze it "complete" while text keeps appending.
    abortLocalOnly(agent, { settleMessages: false });
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
      transportReconnectAttempt: 0,
      replayTruncated: false,
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
        transportReconnectAttempt: 0,
        connectionError: "reconnect-failed",
        streamError: "Соединение потеряно. Не удалось переподключиться.",
      });
      // Item 2: the drop path deliberately preserves "streaming" status so an
      // in-budget reconnect can keep appending text under a live caret. Once
      // the budget is exhausted no further reconnect will ever be attempted
      // for this turn — sweep every live/finishing message still marked
      // "streaming" to "complete" (same idiom as abortLocalOnly's sweep), or
      // the caret blinks forever under the error banner.
      store.set((draft) => {
        const a = draft.agents[agent];
        if (!a) return;
        const src = a.messageSource;
        const msgs = src.mode === "live" || src.mode === "finishing" ? src.messages : [];
        for (const m of msgs) {
          if (m.status === "streaming") m.status = "complete";
        }
      });
      return;
    }

    // Still within budget — show the reconnecting indicator during the wait,
    // and surface the attempt number so the UI can show "attempt N/M" instead
    // of a 30s blind spinner (H8 fix).
    update(agent, {
      connectionPhase: "submitted",
      isLlmReconnecting: true,
      transportReconnectAttempt: attempts,
    });
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
   *
   * `opts.model` (13a) is a ONE-OFF override for this turn only — sent as
   * `body.model` on the /api/chat POST, never persisted, so the next plain
   * send reverts to the agent's configured model.
   */
  async function sendTurn(
    agent: string,
    sessionId: string | null,
    userText: string,
    opts?: { attachments?: Array<MessageAttachment>; userMessageId?: string; model?: string },
  ) {
    const { attachments, userMessageId, model } = opts ?? {};
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
      replayTruncated: false,
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

    // Session-id pre-allocation for NEW chats. When the user starts a fresh
    // chat (`forceNew=true`, no prior `sessionId`), generate the UUID locally
    // and persist it to localStorage BEFORE the POST fires. A refresh during
    // the (potentially slow — URL/attachment enrichment) POST used to leave
    // the UI with no knowledge of the new session; the user's message was
    // lost from their perspective even though the backend eventually created
    // the session. With pre-allocation:
    //   - UI knows the session_id before POST → can saveLastSession immediately
    //   - Backend honours the client-provided id (msg.context.client_session_id
    //     → session_create_new_with_id) so the IDs match end-to-end
    //   - On refresh, localStorage has the session_id → resume finds it in DB
    // For EXISTING chats (`sessionId` already set) the path is unchanged.
    let effectiveSessionId = sessionId;
    if (forceNew && !sessionId) {
      effectiveSessionId = uuid();
      // Mirror agentState.activeSessionId so connect() / subsequent sends
      // reference the same id without waiting for the 202.
      update(agent, {
        activeSessionId: effectiveSessionId,
        forceNewSession: false,
        messageSource: { mode: "live", messages: [userMsg] },
      });
      // Persist BEFORE the POST — refresh-safe.
      saveLastSession(agent, effectiveSessionId);
    }

    if (sessionId) {
      // Existing-session path — leaf_message_id threading unchanged.
      body.session_id = sessionId;
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
    } else if (forceNew && effectiveSessionId) {
      // New-chat pre-allocation: send the id together with force_new_session
      // so the backend creates the session row with THIS UUID (not a
      // server-generated one). Backend's POST handler routes the id through
      // msg.context.client_session_id → session_create_new_with_id.
      body.session_id = effectiveSessionId;
    }
    if (userMessageId) body.user_message_id = userMessageId;
    if (model) body.model = model;
    if (forceNew) {
      body.force_new_session = true;
      // `forceNewSession` was already cleared above for the pre-allocation
      // branch; clear it here too for the existing-session-with-force case
      // (rare but defensive).
      if (!effectiveSessionId || sessionId) {
        update(agent, { forceNewSession: false });
      }
    }

    // ── POST → 202 → connect ──
    try {
      const { session_id } = await startTurn(agent, body);
      // Persist last session (was wired via onSessionId → saveLastSession).
      // For pre-allocated new-chat sessions the returned id SHOULD match
      // `effectiveSessionId` — call saveLastSession anyway so any divergence
      // (e.g. backend ignored the id due to a config gate) self-heals.
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

  // ── Tab-lifecycle recovery (G4) ───────────────────────────────────────
  // Browsers may suspend an SSE socket on a hidden tab; when the tab returns
  // we may have a half-open connection where `reader.read()` is parked and
  // no events are flowing. Three distinct browser signals can mean "the tab
  // is back": `visibilitychange`→visible, `online` (network regained), and
  // `pageshow` (bfcache restore / back-forward navigation). All three funnel
  // into the SAME staleness-checked reattach below — one code path, three
  // triggers. Hiding the tab (`visibilitychange`→hidden) is a STRICT no-op:
  // no dispose, no settle, no state reset. A live stream must never be torn
  // down just because the tab lost focus.
  function reattachStaleAgents(staleMs: number) {
    if (_disposed) return;
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
      // POSTs /abort (tab focus must not cancel the backend turn), settles
      // nothing (settleMessages:false — a reconnect CONTINUATION of the same
      // turn, same message id), and replays the envelope (or an empty
      // finished envelope if the turn already ended while hidden).
      if (last !== 0 && now - last < staleMs) continue;
      try {
        connect(agent, sid);
      } catch (e) {
        if (process.env.NODE_ENV !== "production") {
          console.warn("[streaming-renderer] tab-lifecycle reattach failed", e);
        }
      }
    }
  }

  function handleVisibilityChange() {
    if (_disposed) return;
    if (typeof document === "undefined") return;
    // Hidden branch: NO dispose, NO settle, NO state reset (G4). Only the
    // transition TO visible triggers a reattach check.
    if (document.visibilityState !== "visible") return;
    reattachStaleAgents(VISIBILITY_STALE_MS);
  }

  function handleOnline() {
    reattachStaleAgents(VISIBILITY_STALE_MS);
  }

  function handlePageShow() {
    reattachStaleAgents(VISIBILITY_STALE_MS);
  }

  // C2: focused-tab activity watchdog. The visibility/online/pageshow triggers
  // above only fire on browser lifecycle events, so a half-open socket on a tab
  // that STAYS focused would never be noticed. This timer polls independently
  // and reattaches any active-phase agent silent past ACTIVITY_WATCHDOG_STALE_MS
  // (a laxer threshold than the event-driven path — polling must not thrash a
  // merely-slow LLM).
  let _activityWatchdog: ReturnType<typeof setInterval> | null = null;

  let _visibilityListenerAttached = false;
  function attachLifecycleListeners() {
    if (_visibilityListenerAttached) return;
    if (typeof document === "undefined") return;
    document.addEventListener("visibilitychange", handleVisibilityChange);
    if (typeof window !== "undefined") {
      window.addEventListener("online", handleOnline);
      window.addEventListener("pageshow", handlePageShow);
    }
    if (_activityWatchdog === null) {
      _activityWatchdog = setInterval(() => {
        if (_disposed) return;
        reattachStaleAgents(ACTIVITY_WATCHDOG_STALE_MS);
      }, ACTIVITY_WATCHDOG_INTERVAL_MS);
    }
    _visibilityListenerAttached = true;
  }
  attachLifecycleListeners();

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
   * Full teardown of the renderer: removes the document `visibilitychange`
   * listener plus the window `online`/`pageshow` listeners (the process-wide
   * side effects) and clears every pending debounce timer. Idempotent. After
   * dispose, the tab-lifecycle handlers and the debounced UI-state saver are
   * inert. The live store owns one renderer for
   * the page lifetime, so this is mainly for HMR / tests / any future
   * re-instantiation, none of which should leak a stale listener.
   */
  function dispose() {
    if (_disposed) return;
    _disposed = true;
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    }
    if (typeof window !== "undefined") {
      window.removeEventListener("online", handleOnline);
      window.removeEventListener("pageshow", handlePageShow);
    }
    _visibilityListenerAttached = false;
    if (_activityWatchdog !== null) {
      clearInterval(_activityWatchdog);
      _activityWatchdog = null;
    }
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
