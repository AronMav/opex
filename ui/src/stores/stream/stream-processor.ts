// ── stream/stream-processor.ts ──────────────────────────────────────────────
// Pure SSE event dispatcher. All buffer mutations go through session.buffer.*.
// All store writes go through session.commit() or session.write().

import { sseLog } from "./sse-debug";
import {
  parseContentParts,
  parseSSELines,
  parseSseEvent,
} from "@/stores/sse-events";
import { queryClient } from "@/lib/query-client";
import { qk, flatSessionsFromCache, type SessionsInfiniteData } from "@/lib/queries";
import { useChatStore } from "@/stores/chat-store";
import { getCachedRawMessages } from "../chat-history";

import type {
  ChatMessage,
  TextPart,
  ToolPart,
  ApprovalPart,
  ClarifyPart,
  ConnectionPhase,
  AgentState,
  MessagePart,
} from "../chat-types";
import type { StreamSession } from "../stream-session";

// ── Public interface ──────────────────────────────────────────────────────────

export interface StreamProcessorCallbacks {
  /** Called when a `data-session-id` event arrives. */
  onSessionId: (sid: string) => void;
  /**
   * Read current agent state from the store. Injected to avoid circular import
   * (chat-store → streaming-renderer → stream-processor → chat-store).
   */
  getAgentState: (agent: string) => AgentState | undefined;
  /**
   * Call updateSessionParticipants on the store. Injected for the same reason.
   */
  updateSessionParticipants: (sessionId: string, participants: string[]) => void;
  /**
   * Fix #6: invoked on every SSE event so the renderer can detect a stalled
   * (tab-throttled) socket via a visibilitychange listener. Optional — purely
   * for stale-detection telemetry.
   */
  onEventActivity?: () => void;

  // ── Batch-apply transport (chat-stream.ts / TurnStreamCallbacks) ────

  /** Fired after `sync_end` — the replayed envelope has been committed to the store as a single batch. */
  onEnvelopeApplied?: () => void;
  /** Fired when the turn is authoritatively over: an explicit `finish` event, or
   *  a `sync_begin` whose `runStatus` was already terminal (finished/error/
   *  interrupted) with no further live events before the stream closed. */
  onFinished?: () => void;
  /**
   * Fired when the connection drops WITHOUT a terminal signal — this module
   * does not retry; the caller decides whether/how to re-open
   * (streaming-renderer's `connect` re-opens the same envelope). NOT fired
   * when the turn ended in error: an `error`/`sync`-error event leaves
   * connectionPhase="error" and the finally block settles without re-opening.
   */
  onConnectionLost?: () => void;
}

export interface StreamProcessorOpts {
  sessionId: string | null;
  callbacks: StreamProcessorCallbacks;
}

// ── Core processor ─────────────────────────────────────────────────────────────

export async function processSSEStream(
  session: StreamSession,
  body: ReadableStream<Uint8Array>,
  opts: StreamProcessorOpts,
): Promise<void> {
  const { sessionId: knownSessionId, callbacks } = opts;
  const agent = session.agent;

  const reader = body.getReader();
  const decoder = new TextDecoder();
  const lineBuffer = { current: "" };

  // Reset buffer for this stream (preserves currentRespondingAgent = agent name)
  session.buffer.reset();

  let receivedSessionId: string | null = knownSessionId ?? null;
  let receivedFinishEvent = false;

  // Batch-apply state: sync_begin..sync_end gates the throttled per-event
  // commit so a whole replayed envelope lands as a single store commit at
  // sync_end.
  let batching = false;
  let lastRunStatus: string | null = null;

  /**
   * Gate for the throttled per-event commit. While replaying inside a
   * sync_begin..sync_end envelope, defer to the single `session.commit()`
   * that `sync_end` performs — this is the "batch" behavior. Does NOT gate
   * `session.commit()` call sites (step-start, finish) — those commit per-LLM-
   * iteration/on-finish regardless, which is correct: a multi-step replay
   * still commits once per step, and the final commit() at sync_end flushes
   * whatever remains after the last step.
   */
  function maybeScheduleCommit(): void {
    if (batching) return;
    session.scheduleCommit();
  }

  try {
    while (true) {
      if (session.signal.aborted) break;
      const { done, value } = await reader.read();
      if (done) break;

      const chunk = decoder.decode(value, { stream: true });
      const lines = parseSSELines(chunk, lineBuffer);

      for (const line of lines) {
        // Standard SSE `id:` field — carries the envelope seq (replay
        // ordering within the sync_begin/sync_end envelope), not a
        // Last-Event-ID resumption token. The GET stream always returns the
        // full envelope regardless of any Last-Event-ID header, so seq is
        // not tracked client-side here; fall through to the non-data skip
        // below.
        if (!line.startsWith("data:")) continue;
        const raw = line.slice(5).trim();
        if (raw === "[DONE]") continue;

        const event = parseSseEvent(raw);
        if (!event) {
          if (process.env.NODE_ENV !== "production") console.warn("[sse] unparseable event:", raw.slice(0, 120));
          continue;
        }

        if (session.signal.aborted || !session.isCurrent) {
          continue;
        }

        // Fix #6: record event activity so the visibilitychange listener
        // in streaming-renderer can detect a stalled socket on tab focus.
        callbacks.onEventActivity?.();

        // Debug tracing — see ./sse-debug.ts. No-op when not enabled.
        sseLog(agent, event.type, sseEventBrief(event));

        switch (event.type) {
          case "data-session-id": {
            const sid = event.data.sessionId;
            if (sid && session.isCurrent) {
              receivedSessionId = sid;
              // SSE-03: Confirm the optimistic user message
              session.writeDraft((agentDraft: AgentState) => {
                if (agentDraft.messageSource.mode !== "live") return;
                const msgs = agentDraft.messageSource.messages;
                for (let i = msgs.length - 1; i >= 0; i--) {
                  if (msgs[i].role === "user" && msgs[i].status === "sending") {
                    msgs[i].status = "confirmed";
                    break;
                  }
                }
              });
              const contextLimit = event.data.contextLimit;
              session.writeDraft((agentDraft: AgentState) => {
                agentDraft.activeSessionId = sid;
                if (contextLimit != null) agentDraft.modelContextLimit = contextLimit;
                // Fix H (new-chat case): a message queued (F085) BEFORE this turn's
                // session id was known is stamped `pendingMessage.sessionId = null`
                // (see queueMessage). Now that the turn's real session id has
                // arrived, sync the stamp to it — same-turn session assignment,
                // not a context switch — so ChatThread's drain-effect stamp check
                // (sessionId must match activeSessionId) sees a match instead of
                // false-discarding the queued message. A pending item already
                // stamped with a concrete sessionId (queued while resumed into an
                // existing session) is left untouched: that's a genuine
                // later-switch case the drain effect must still catch.
                if (agentDraft.pendingMessage && agentDraft.pendingMessage.sessionId == null) {
                  agentDraft.pendingMessage.sessionId = sid;
                }
              });
              // Persist so the value survives page refresh (restored on agent init).
              if (contextLimit != null) {
                try { localStorage.setItem(`ctx_limit:${agent}`, String(contextLimit)); } catch { }
              }
              callbacks.onSessionId(sid);

              const cachedSession = flatSessionsFromCache(
                queryClient.getQueryData<SessionsInfiniteData>(qk.sessions(agent)),
              ).find(s => s.id === sid);
              if (cachedSession?.participants) {
                callbacks.updateSessionParticipants(sid, cachedSession.participants);
              }
            }
            break;
          }

          case "start": {
            // Capture id before reset() generates a new one
            const preservedId = event.messageId || session.buffer.assistantId;
            const partsBefore = session.buffer.parts.length;
            session.buffer.reset();
            session.buffer.assistantId = preservedId;
            if (event.agentName) session.buffer.currentRespondingAgent = event.agentName;
            sseLog(agent, "post-start-reset", { assistantId: preservedId, partsBeforeReset: partsBefore });
            break;
          }

          case "text-start": {
            if (event.agentName) session.buffer.currentRespondingAgent = event.agentName;
            session.writeDraft((agentDraft: AgentState) => {
              agentDraft.isLlmReconnecting = false;
            });
            break;
          }

          case "text-delta": {
            session.buffer.parser.processDelta(event.delta);
            maybeScheduleCommit();
            break;
          }

          case "text-end": {
            // Close the text block immediately: flush parser accumulator into
            // parts and clear insideThink. Without this the parser keeps any
            // held-back chars in accum and any open <think> state across LLM
            // iteration boundaries — which produced "iter0_text + iter1_tail"
            // mid-word concatenation and made step-boundary fall in the wrong
            // place inside the merged text.
            session.buffer.endTextBlock();
            sseLog(agent, "post-text-end-buffer", partsBrief(session.buffer.parts));
            maybeScheduleCommit();
            break;
          }

          case "tool-input-start": {
            session.buffer.flushText();
            const { toolCallId: tcId, toolName: tcName } = event;
            session.buffer.toolInputChunks.set(tcId, []);
            const toolPart: ToolPart = {
              type: "tool",
              toolCallId: tcId,
              toolName: tcName,
              state: "input-streaming",
              input: {},
            };
            session.buffer.parts.push(toolPart);
            sseLog(agent, "post-tool-input-start-buffer", partsBrief(session.buffer.parts));
            maybeScheduleCommit();
            break;
          }

          case "tool-input-delta": {
            const { toolCallId: tcId, inputTextDelta: delta } = event;
            if (delta) session.buffer.toolInputChunks.get(tcId)?.push(delta);
            break;
          }

          case "tool-input-available": {
            const { toolCallId: tcId, input } = event;
            session.buffer.toolInputChunks.delete(tcId);
            const idx = session.buffer.parts.findIndex(
              p => p.type === "tool" && (p as ToolPart).toolCallId === tcId,
            );
            if (idx >= 0) {
              session.buffer.parts[idx] = {
                ...(session.buffer.parts[idx] as ToolPart),
                state: "input-available",
                input: (input as Record<string, unknown>) ?? {},
              };
            }
            maybeScheduleCommit();
            break;
          }

          case "tool-output-available": {
            const { toolCallId: tcId, output } = event;
            const idx = session.buffer.parts.findIndex(
              p => p.type === "tool" && (p as ToolPart).toolCallId === tcId,
            );
            if (idx >= 0) {
              session.buffer.parts[idx] = {
                ...(session.buffer.parts[idx] as ToolPart),
                state: "output-available",
                output,
              };
            }
            maybeScheduleCommit();
            break;
          }

          case "tool-approval-needed": {
            session.buffer.flushText();
            const approval: ApprovalPart = {
              type: "approval",
              approvalId: event.approvalId,
              toolName: event.toolName,
              toolInput: (event.toolInput ?? {}) as Record<string, unknown>,
              timeoutMs: event.timeoutMs,
              receivedAt: Date.now(),
              status: "pending",
            };
            session.buffer.parts.push(approval);
            maybeScheduleCommit();
            break;
          }

          case "tool-approval-resolved": {
            const idx = session.buffer.parts.findIndex(
              p => p.type === "approval" && (p as ApprovalPart).approvalId === event.approvalId,
            );
            if (idx >= 0) {
              const existing = session.buffer.parts[idx] as ApprovalPart;
              // event.action is the typed ApprovalAction union — strict subset of
              // ApprovalPart["status"]; assigns directly without a runtime check.
              session.buffer.parts[idx] = {
                ...existing,
                status: event.action,
                modifiedInput: (event.modifiedInput ?? undefined) as Record<string, unknown> | undefined,
              };
            }
            maybeScheduleCommit();
            break;
          }

          case "clarify-needed": {
            session.buffer.flushText();
            const clarify: ClarifyPart = {
              type: "clarify",
              clarifyId: event.clarifyId,
              question: event.question,
              choices: event.choices,
              timeoutMs: event.timeoutMs,
              receivedAt: Date.now(),
              response: null,
            };
            session.buffer.parts.push(clarify);
            maybeScheduleCommit();
            break;
          }

          case "step-start": {
            // Each LLM tool-loop iteration is a separate live ChatMessage with
            // a pre-allocated UUID that matches its eventual DB row id (sent
            // as `messageId` in the step-start event). On every step-start we
            // commit the current buffer as a finished ChatMessage and reset
            // the buffer to start accumulating the next iteration under its
            // own id.
            //
            // Iteration 0 special case: backend also emits MessageStart with
            // the same id for backward compatibility. The `start` handler
            // already reset the buffer with that id, so this step-start is a
            // no-op — skip without an extra reset.
            if (!event.messageId) {
              sseLog(agent, "step-start-no-message-id");
              break;
            }
            if (session.buffer.assistantId === event.messageId) {
              sseLog(agent, "step-start-already-on-id", { id: event.messageId });
              break;
            }
            const prevIterationId = session.buffer.assistantId;
            if (session.buffer.snapshot().length > 0) {
              session.commit();
            }
            session.buffer.reset();
            session.buffer.assistantId = event.messageId;
            // Settle the PREVIOUS iteration's message: it stopped receiving
            // text the moment the buffer switched ids, so its inline caret
            // must stop now — `finish` only sweeps at end-of-turn, and the
            // "calling tool" bubble must not blink for the whole tool run.
            session.writeDraft((agentDraft: AgentState) => {
              const src = agentDraft.messageSource;
              const liveMessages: ChatMessage[] =
                src.mode === "live" || src.mode === "finishing" ? src.messages : [];
              const prev = liveMessages.find((m) => m.id === prevIterationId);
              if (prev && prev.status === "streaming") prev.status = "complete";
            });
            sseLog(agent, "step-start-new-message", { id: event.messageId });
            break;
          }
          // `step-finish` does not exist on the wire (removed in S6.5).

          case "file": {
            session.buffer.flushText();
            session.buffer.parts.push({
              type: "file",
              url: event.url,
              mediaType: event.mediaType || "application/octet-stream",
            });
            maybeScheduleCommit();
            break;
          }

          case "rich-card": {
            session.buffer.flushText();
            // F077: the Rust SSE wire wraps every non-table/metric card as
            // RichCardData::Other → {cardType:"other", data:{cardType:<real>,
            // data:<flat>}}. The history/raw-marker path (chat-history.ts) stores
            // the FLAT shape, so unwrap the Other nesting here to match — without
            // it, custom cards (e.g. the shipping handler_menu file-handler menu)
            // render blank on the live SSE path and only appear after a reload.
            let cardType: string = event.cardType;
            let data: Record<string, unknown> = event.data as Record<string, unknown>;
            if (cardType === "other" && data && typeof data.cardType === "string") {
              cardType = data.cardType;
              data = (data.data ?? {}) as Record<string, unknown>;
            }
            session.buffer.parts.push({
              type: "rich-card",
              cardType,
              data,
            });
            maybeScheduleCommit();
            break;
          }

          case "sync": {
            const { content: syncContent, status: syncStatus } = event;
            const syncParts: MessagePart[] = parseContentParts(syncContent || "");
            // Fix #5: "interrupted" (Pi restarted mid-stream — emitted by
            // api_chat_resume_stream after error_job marks a stale 'running'
            // job) must surface as an error so the user sees the banner. The
            // previous mapping silently classified it as "streaming", which
            // left the chat looking healthy while the run was actually dead.
            // Wire-level SyncStatus = "finished" | "error" | "interrupted" | "running"
            // (legacy "done"/"failed" were renamed in S6.5 — see resume.rs:47-59).
            const isErrorLike = syncStatus === "error" || syncStatus === "interrupted";
            const phase: ConnectionPhase =
              isErrorLike ? "error" :
                syncStatus === "finished" ? "idle" : "streaming";
            const errorText = isErrorLike ? (event.error ?? null) : null;
            // Message status mirrors "is the turn still live" — only "running"
            // keeps the caret going; finished/error/interrupted all settle to
            // "complete" so a dead or done turn never keeps blinking.
            const msgStatus: "streaming" | "complete" =
              syncStatus === "running" ? "streaming" : "complete";

            // Single writeDraft — message + connectionPhase + error fields are atomic (fixes bugs b and c)
            session.writeDraft((agentDraft: AgentState) => {
              if (receivedSessionId && agentDraft.activeSessionId && receivedSessionId !== agentDraft.activeSessionId) return;

              const currentSessionId = agentDraft.activeSessionId;
              const isSameSession = receivedSessionId && currentSessionId === receivedSessionId;

              // Only skip the live-mode switch when we're in "history" mode for the same session.
              // For "new-chat" mode we always need to switch, even if session IDs match.
              // "finishing" mode must also be preserved — its frozen messages must not be discarded.
              const src = agentDraft.messageSource;
              const isHistoryMode = src.mode === "history";
              const isLiveOrFinishing = src.mode === "live" || src.mode === "finishing";
              if (!isLiveOrFinishing && !(isHistoryMode && isSameSession)) {
                agentDraft.messageSource = { mode: "live", messages: [] };
              }

              // After the possible switch, pull the live messages array. Only
              // live/finishing carry messages; the preserved history-same-session
              // branch has none, so liveMessages is empty there (matching the
              // previous `as any[]` read which yielded undefined → findIndex -1).
              const resolvedSrc = agentDraft.messageSource;
              const liveMessages: ChatMessage[] =
                resolvedSrc.mode === "live" || resolvedSrc.mode === "finishing"
                  ? resolvedSrc.messages
                  : [];
              const existingIdx = liveMessages.findIndex((m) => m.id === session.buffer.assistantId);

              if (existingIdx >= 0) {
                const existingMsg = liveMessages[existingIdx];
                const localTextLen = existingMsg.parts
                  .filter((p): p is TextPart => p.type === "text")
                  .reduce((acc, p) => acc + (p.text?.length ?? 0), 0);
                const syncTextLen = syncParts
                  .filter((p): p is TextPart => p.type === "text")
                  .reduce((acc, p) => acc + (p.text?.length ?? 0), 0);
                if (syncTextLen > localTextLen || Math.abs(syncTextLen - localTextLen) > 50) {
                  // H2: syncParts carries only text/reasoning — preserve tool, approval, file parts
                  const preserved = existingMsg.parts.filter(
                    (p) => p.type !== "text" && p.type !== "reasoning"
                  );
                  existingMsg.parts = [...syncParts, ...preserved];
                  sseLog(agent, "sync-replaced-parts", { reason: "text-len-diff", localLen: localTextLen, syncLen: syncTextLen, partsBrief: partsBrief(existingMsg.parts) });
                } else {
                  sseLog(agent, "sync-noop", { localLen: localTextLen, syncLen: syncTextLen });
                }
                if (existingMsg.status !== "complete") {
                  existingMsg.status = msgStatus;
                }
              } else {
                liveMessages.push({
                  id: session.buffer.assistantId,
                  role: "assistant",
                  parts: syncParts,
                  createdAt: session.buffer.assistantCreatedAt,
                  agentId: session.buffer.currentRespondingAgent ?? undefined,
                  status: msgStatus,
                });
              }

              // Atomic: phase + error in same writeDraft
              if (agentDraft.connectionPhase !== "error" || phase === "error") {
                agentDraft.connectionPhase = phase;
              }
              if (errorText !== null) {
                agentDraft.streamError = errorText;
                agentDraft.connectionError = errorText;
              }
            });
            break;
          }

          // Envelope boundary markers (sync_begin..sync_end wrap the replay
          // of a resumed/new stream — see gateway/handlers/chat/stream.rs).
          case "sync_begin": {
            // `runStatus` gates the finished-vs-dropped decision in the finally
            // block below. `boundaryMessageId` is intentionally ignored — the
            // client render is id-keyed (see selectRenderMessages/mergeRender),
            // not positionally boundary-sliced. The server still emits it.
            lastRunStatus = event.runStatus;
            if (event.truncated) session.write({ replayTruncated: true });
            session.buffer.reset();
            batching = true;
            break;
          }

          case "sync_end": {
            batching = false;
            // Only flush if the buffer actually accumulated content during this
            // envelope. A DB-branch resume never touches the buffer — the `sync`
            // event above writes resumed content directly into messageSource — so
            // committing here unconditionally would overwrite that message's parts
            // with an empty snapshot, blanking the resumed content. Mirrors the
            // same guard in the `finally` block below.
            if (session.buffer.snapshot().length > 0) session.commit();
            callbacks.onEnvelopeApplied?.();
            break;
          }

          case "usage": {
            // Write input/output + extended fields to AgentState so the ContextBar
            // can render a breakdown tooltip. Extended fields are subsets of
            // input/output (NOT additive) — see TokenUsage doc in opex-types.
            //
            // Route by event.agentName, not by the session's bound agent. In a
            // multi-agent flow (AgentSwitch / cron-driven peer responses), a
            // usage event from agent B can arrive on agent A's stream — without
            // routing it would overwrite A's tokenUsage state with B's numbers
            // and corrupt A's ContextBar / billing breakdown.
            //
            // Falls back to session.writeDraft (current-agent semantics) when
            // older backends don't tag the event — preserves single-agent behavior.
            const targetAgent = event.agentName;
            if (targetAgent && targetAgent !== session.agent) {
              useChatStore.setState((draft: { agents: Record<string, AgentState> }) => {
                const st = draft.agents[targetAgent];
                if (!st) return;
                st.contextTokens = event.inputTokens;
                st.contextOutputTokens = event.outputTokens;
                st.cacheReadTokens = event.cacheReadTokens ?? null;
                st.cacheCreationTokens = event.cacheCreationTokens ?? null;
                st.reasoningTokens = event.reasoningTokens ?? null;
              });
            } else {
              session.writeDraft((agentDraft: AgentState) => {
                agentDraft.contextTokens = event.inputTokens;
                agentDraft.contextOutputTokens = event.outputTokens;
                agentDraft.cacheReadTokens = event.cacheReadTokens ?? null;
                agentDraft.cacheCreationTokens = event.cacheCreationTokens ?? null;
                agentDraft.reasoningTokens = event.reasoningTokens ?? null;
              });
            }
            break;
          }

          case "reconnecting": {
            session.writeDraft((agentDraft: AgentState) => {
              agentDraft.isLlmReconnecting = true;
            });
            break;
          }

          case "finish": {
            receivedFinishEvent = true;
            session.cancelScheduledCommit();
            session.buffer.flushText();
            session.commit("streaming");  // final snapshot of all parts; "error" guard in commit() prevents overwrite
            session.buffer.reset();       // clean buffer for next LLM iteration

            // The turn is over: SWEEP the live/finishing messages and settle
            // every status === "streaming" to "complete" so the inline caret
            // (TextPart, keyed on status === "streaming") stops blinking on
            // ALL iterations — not just the final one (step-start settles
            // earlier iterations eagerly; this covers any stragglers).
            // Unconditional — not gated on receivedSessionId.
            session.writeDraft((agentDraft: AgentState) => {
              const src = agentDraft.messageSource;
              const liveMessages: ChatMessage[] =
                src.mode === "live" || src.mode === "finishing" ? src.messages : [];
              for (const m of liveMessages) {
                if (m.status === "streaming") m.status = "complete";
              }
            });

            if (receivedSessionId) {
              const sid = receivedSessionId;
              session.writeDraft((agentDraft: AgentState) => {
                agentDraft.isLlmReconnecting = false;
                agentDraft.activeSessionIds = (agentDraft.activeSessionIds || []).filter((id: string) => id !== sid);
              });
            }
            break;
          }

          case "error": {
            const errText = event.errorText;
            if (errText.includes("turn limit") || errText.includes("cycle detected")) {
              session.write({ turnLimitMessage: errText });
            } else {
              session.writeDraft((agentDraft: AgentState) => {
                agentDraft.streamError = errText;
                agentDraft.connectionError = errText;
                agentDraft.isLlmReconnecting = false;
                agentDraft.connectionPhase = "error";
                // Sweep: settle EVERY live message still marked "streaming" —
                // an error can land after step-start switched buffer ids, so
                // targeting only the current id would leave earlier
                // iterations' carets blinking on a dead turn.
                const src = agentDraft.messageSource;
                const liveMessages: ChatMessage[] =
                  src.mode === "live" || src.mode === "finishing" ? src.messages : [];
                for (const m of liveMessages) {
                  if (m.status === "streaming") m.status = "complete";
                }
              });
            }
            break;
          }
        }
      }
    }
  } finally {
    reader.releaseLock();
    session.cancelScheduledCommit();
    session.buffer.flushText();

    if (!session.signal.aborted) {
      // Flush any remaining buffer content
      if (session.buffer.snapshot().length > 0) session.commit();

      const agentState = callbacks.getAgentState(agent);
      const isError = agentState?.connectionPhase === "error";

      // The batch-apply transport has no reconnect loop inside this module —
      // the caller (chat-stream.ts's openTurnStream → streaming-renderer's
      // `connect`) owns re-open policy. Decide only whether the turn is
      // authoritatively over (explicit `finish`, or a `sync_begin` whose
      // runStatus was already terminal) vs. a connection drop mid-turn.
      // This is a lifecycle signal only — NOT a substitute for the
      // error/interrupted UI state, which is derived from the replayed
      // `error`/`sync` events above regardless of this decision.
      const isTerminalRunStatus =
        lastRunStatus === "finished" || lastRunStatus === "error" || lastRunStatus === "interrupted";
      if (isError) {
        // Turn ended in error (replayed `error`/`sync`-error event) — this is
        // NOT a connection drop. Leave connectionPhase="error"; do not
        // re-open (onConnectionLost) or fire onFinished (which would idle the
        // phase). Fall through to the finishing→history settle below.
      } else if (!receivedFinishEvent && !isTerminalRunStatus) {
        callbacks.onConnectionLost?.();
        return;
      } else {
        callbacks.onFinished?.();
      }

      // Turn is terminal (error / finish / terminal runStatus — the
      // connection-drop branch returned above). Sweep any live message still
      // marked "streaming": the leftover-buffer commit() just above can land
      // a message AFTER the in-handler sweeps ran (e.g. an error arriving
      // before the throttled commit of the current iteration), and a
      // terminal turn must never leave a blinking caret.
      session.writeDraft((agentDraft: AgentState) => {
        const src = agentDraft.messageSource;
        const liveMessages: ChatMessage[] =
          src.mode === "live" || src.mode === "finishing" ? src.messages : [];
        for (const m of liveMessages) {
          if (m.status === "streaming") m.status = "complete";
        }
      });

      if (!isError) {
        // T7: "complete" folded into "idle". The finishing→history dance below
        // keeps the assistant visible during the refetch window (frozen live
        // overlay), so no distinct transient phase is needed.
        session.write({ connectionPhase: "idle", connectionError: null, isLlmReconnecting: false });
      }
    } else {
      // Abort case: commit any partial parts (commit() drops silently if not current)
      session.commit("streaming");
    }
  }

  // Post-finally: switch to finishing mode first, await RQ refetch, then history.
  if (!session.signal.aborted) {
    if (receivedSessionId) {
      callbacks.onSessionId(receivedSessionId);
    }

    const completedSessionId =
      receivedSessionId ?? callbacks.getAgentState(agent)?.activeSessionId;

    if (completedSessionId) {
      // Step 1: freeze live messages in "finishing" mode so they stay visible
      // while React Query fetches fresh data. The assistant response remains
      // on screen during the refetch window instead of flashing out.
      const agentState = callbacks.getAgentState(agent);
      const frozenLive =
        agentState?.messageSource.mode === "live"
          ? agentState.messageSource.messages
          : [];

      session.write({
        messageSource: {
          mode: "finishing" as const,
          sessionId: completedSessionId,
          messages: frozenLive,
        },
      });

      // Step 2: invalidate sessions list (non-blocking — just marks stale)
      queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });

      // Step 3: refetchQueries waits for the network request to complete
      // regardless of subscriber state. invalidateQueries with refetchType:"active"
      // (the default) resolves immediately if useSessionMessages is not mounted,
      // which can happen during a tab switch — making refetchQueries safer here.
      await queryClient.refetchQueries({
        queryKey: qk.sessionMessages(completedSessionId),
      });

      // Step 4 (bug D): settle to history ONLY when the refetched cache actually
      // contains the turn's assistant row. With the id-keyed merge render the
      // frozen assistant stays visible via the live overlay until history has
      // it; forcing history unconditionally here would flash the assistant out
      // when the row is not yet persisted under the same id (aborted/partial
      // turns, or a lagging read replica). If the assistant id is not present
      // we leave the overlay in "finishing" — mergeRender keeps it on screen,
      // and ChatThread's id-guarded finalizeHandoff (same guard) completes the
      // switch once the row lands. When there is no assistant to protect
      // (e.g. an error before any assistant text) we settle immediately.
      const assistantId = [...frozenLive].reverse().find((m) => m.role === "assistant")?.id;
      const rowsAfterRefetch = getCachedRawMessages(completedSessionId, agent);
      const assistantPersisted =
        !assistantId || rowsAfterRefetch.some((r) => r.id === assistantId);
      if (assistantPersisted) {
        session.write({
          messageSource: { mode: "history" as const, sessionId: completedSessionId },
        });
      }
    } else {
      queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
    }
  }
}

// ── Debug helpers ────────────────────────────────────────────────────────────

/** Compact summary of buffer.parts for debug logs. */
function partsBrief(parts: MessagePart[]): { count: number; types: string; texts: { len: number; head: string }[] } {
  const types = parts.map((p) => p.type[0]).join("");
  const texts = parts
    .filter((p): p is TextPart => p.type === "text")
    .map((p) => ({ len: p.text.length, head: p.text.slice(0, 40) }));
  return { count: parts.length, types, texts };
}

/** Brief representation of an SSE event for debug logging. */
function sseEventBrief(event: { type: string } & Record<string, unknown>): Record<string, unknown> {
  switch (event.type) {
    case "text-delta":
      return { id: event.id, delta: typeof event.delta === "string" ? event.delta.slice(0, 80) : "" };
    case "text-start":
    case "text-end":
      return { id: event.id };
    case "step-start":
      return { stepId: event.stepId };
    case "start":
      return { messageId: event.messageId };
    case "tool-input-start":
      return { toolCallId: event.toolCallId, toolName: event.toolName };
    case "tool-input-available":
      return { toolCallId: event.toolCallId };
    case "tool-output-available":
      return { toolCallId: event.toolCallId };
    case "sync": {
      const content = typeof event.content === "string" ? event.content : "";
      const calls = Array.isArray(event.toolCalls) ? event.toolCalls.length : 0;
      return { contentLen: content.length, contentHead: content.slice(0, 60), toolCalls: calls, status: event.status };
    }
    case "finish":
      return {};
    case "data-session-id": {
      const data = event.data as { sessionId?: string } | undefined;
      return { sessionId: data?.sessionId };
    }
    default:
      return {};
  }
}
