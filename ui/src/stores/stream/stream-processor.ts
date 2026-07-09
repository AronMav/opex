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
import { qk } from "@/lib/queries";
import { useChatStore } from "@/stores/chat-store";
import type { SessionRow } from "@/types/api";

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
   * Called when the stream ends without a finish event — the caller decides
   * the reconnect policy (scheduleReconnect in streaming-renderer.ts).
   */
  onReconnectNeeded: (sid: string, attempt: number) => void;
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
   * Called when the stream ends cleanly (not aborted, not reconnect). Used by
   * the renderer to persist UI state (debounced PATCH to backend).
   */
  onStreamDone?: () => void;
  /**
   * Fix #6: invoked on every SSE event so the renderer can detect a stalled
   * (tab-throttled) socket via a visibilitychange listener. Optional — purely
   * for stale-detection telemetry.
   */
  onEventActivity?: () => void;
}

export interface StreamProcessorOpts {
  sessionId: string | null;
  reconnectAttempt: number;
  callbacks: StreamProcessorCallbacks;
}

// ── Core processor ─────────────────────────────────────────────────────────────

export async function processSSEStream(
  session: StreamSession,
  body: ReadableStream<Uint8Array>,
  opts: StreamProcessorOpts,
): Promise<void> {
  const { sessionId: knownSessionId, reconnectAttempt, callbacks } = opts;
  const agent = session.agent;

  const reader = body.getReader();
  const decoder = new TextDecoder();
  const lineBuffer = { current: "" };

  // Reset buffer for this stream (preserves currentRespondingAgent = agent name)
  session.buffer.reset();

  let receivedSessionId: string | null = knownSessionId ?? null;
  let receivedFinishEvent = false;

  try {
    while (true) {
      if (session.signal.aborted) break;
      const { done, value } = await reader.read();
      if (done) break;

      const chunk = decoder.decode(value, { stream: true });
      const lines = parseSSELines(chunk, lineBuffer);

      for (const line of lines) {
        // Standard SSE id field — backend emits it for every buffered event.
        // Stash on the session AND in agent state so reconnect can pass
        // Last-Event-ID even after StreamSession disposal (Phase 3 offset
        // tracking).
        if (line.startsWith("id:")) {
          const idStr = line.slice(3).trim();
          const idNum = Number.parseInt(idStr, 10);
          if (!Number.isNaN(idNum)) {
            // F054: the backend attaches an `id:` to EVERY buffered event
            // (incl. every text-delta). Do NOT write the store here — that was
            // ~1 immer produce per token, defeating the 50ms commit throttle and
            // making every subscriber selector re-run per token (O(N^2) on long
            // replies). Keep it on the session only; it is flushed to agent state
            // once per stream in the finally block below (the reconnect path reads
            // the store copy).
            session.lastEventId = idNum;
          }
          continue;
        }
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
              session.write({
                activeSessionId: sid,
                ...(contextLimit != null && { modelContextLimit: contextLimit }),
              });
              // Persist so the value survives page refresh (restored on agent init).
              if (contextLimit != null) {
                try { localStorage.setItem(`ctx_limit:${agent}`, String(contextLimit)); } catch { }
              }
              callbacks.onSessionId(sid);

              const sessionsData = queryClient.getQueryData<{ sessions: SessionRow[] }>(
                qk.sessions(agent)
              );
              const cachedSession = sessionsData?.sessions.find(s => s.id === sid);
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
            session.scheduleCommit();
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
            session.scheduleCommit();
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
            session.scheduleCommit();
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
            session.scheduleCommit();
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
            session.scheduleCommit();
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
            session.scheduleCommit();
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
            session.scheduleCommit();
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
            session.scheduleCommit();
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
            if (session.buffer.snapshot().length > 0) {
              session.commit();
            }
            session.buffer.reset();
            session.buffer.assistantId = event.messageId;
            sseLog(agent, "step-start-new-message", { id: event.messageId });
            break;
          }
          // Note: `step-finish` was removed from SseEvent in S6.5 — the
          // server-side StreamEvent::StepFinish is `continue`-skipped and never
          // reaches the wire, so no client-side handling is needed.

          case "file": {
            session.buffer.flushText();
            session.buffer.parts.push({
              type: "file",
              url: event.url,
              mediaType: event.mediaType || "application/octet-stream",
            });
            session.scheduleCommit();
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
            session.scheduleCommit();
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
                  existingMsg.status = syncStatus === "finished" ? "complete" : "streaming";
                }
              } else {
                liveMessages.push({
                  id: session.buffer.assistantId,
                  role: "assistant",
                  parts: syncParts,
                  createdAt: session.buffer.assistantCreatedAt,
                  agentId: session.buffer.currentRespondingAgent ?? undefined,
                  status: syncStatus === "finished" ? "complete" : "streaming",
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
              session.write({ streamError: errText, connectionPhase: "error", connectionError: errText, isLlmReconnecting: false });
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

    // F054: flush the last SSE event id to agent state once per stream (not per
    // event). The store copy is only read on the reconnect/resume path
    // (streaming-renderer reads agents[agent].lastEventId for the Last-Event-ID
    // header). Runs before onReconnectNeeded below so the reconnect sees it.
    if (session.lastEventId !== null) {
      session.write({ lastEventId: session.lastEventId });
    }

    if (!session.signal.aborted) {
      // Flush any remaining buffer content
      if (session.buffer.snapshot().length > 0) session.commit();

      const agentState = callbacks.getAgentState(agent);
      const isError = agentState?.connectionPhase === "error";
      const isIdle = agentState?.connectionPhase === "idle";
      const effectiveSessionId = receivedSessionId ?? agentState?.activeSessionId;

      if (!isError && !isIdle && !receivedFinishEvent && effectiveSessionId) {
        // SSE connection dropped before finish — LLM retry may have been in progress.
        // Reset the flag now; scheduleReconnect will re-set it only if a __reconnecting__
        // chunk arrives on the next SSE connection.
        session.write({ isLlmReconnecting: false });
        callbacks.onReconnectNeeded(effectiveSessionId, reconnectAttempt);
        return;
      }

      if (!isError) {
        // Use "complete" (not "idle") when the stream finished normally so the
        // auto-resume effect in ChatThread cannot fire during the refetch window.
        // "idle" is restored once we transition to history mode below.
        const finishedPhase = receivedFinishEvent ? ("complete" as const) : ("idle" as const);
        session.write({ connectionPhase: finishedPhase, connectionError: null, reconnectAttempt: 0, isLlmReconnecting: false });
      }
      callbacks.onStreamDone?.();
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

      // Step 4: RQ cache now has the fresh exchange — safe to switch to history.
      // Restore "idle" only when transitioning from "complete" (clean finish).
      // Do NOT overwrite "error" — the UI must keep showing the error state.
      const phaseAfterRefetch = callbacks.getAgentState(agent)?.connectionPhase;
      session.write({
        messageSource: { mode: "history" as const, sessionId: completedSessionId },
        ...(phaseAfterRefetch === "complete" ? { connectionPhase: "idle" as const } : {}),
      });
    } else {
      queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
      // No session to refetch — still clear "complete" so the UI input is re-enabled.
      if (callbacks.getAgentState(agent)?.connectionPhase === "complete") {
        session.write({ connectionPhase: "idle" as const });
      }
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
