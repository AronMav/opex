// ── stream/stream-processor.ts ──────────────────────────────────────────────
// Pure SSE event dispatcher. All buffer mutations go through session.buffer.*.
// All store writes go through session.commit() or session.write().

import { parseSSELines, parseSseEvent } from "./sse-parser";
import { parseContentParts } from "@/stores/sse-events";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";
import type { SessionRow } from "@/types/api";

import type {
  ChatMessage,
  TextPart,
  ToolPart,
  ApprovalPart,
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

        switch (event.type) {
          case "data-session-id": {
            const sid = event.data.sessionId;
            if (sid && session.isCurrent) {
              receivedSessionId = sid;
              // SSE-03: Confirm the optimistic user message
              session.writeDraft((agentDraft: AgentState) => {
                if (agentDraft.messageSource.mode !== "live") return;
                const msgs = (agentDraft.messageSource as any).messages as ChatMessage[];
                for (let i = msgs.length - 1; i >= 0; i--) {
                  if (msgs[i].role === "user" && msgs[i].status === "sending") {
                    msgs[i].status = "confirmed";
                    break;
                  }
                }
              });
              session.write({ activeSessionId: sid });
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
            session.buffer.reset();
            session.buffer.assistantId = preservedId;
            if (event.agentName) session.buffer.currentRespondingAgent = event.agentName;
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
              toolInput: event.toolInput,
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
              session.buffer.parts[idx] = {
                ...existing,
                status: event.action,
                modifiedInput: event.modifiedInput,
              };
            }
            session.scheduleCommit();
            break;
          }

          case "step-start":
          case "step-finish":
            // Step groups removed — tools render as flat parts (matching history view)
            break;

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
            session.buffer.parts.push({
              type: "rich-card",
              cardType: event.cardType,
              data: event.data,
            });
            session.scheduleCommit();
            break;
          }

          case "sync": {
            const { content: syncContent, status: syncStatus } = event;
            const syncParts: MessagePart[] = parseContentParts(syncContent || "");
            const phase: ConnectionPhase =
              syncStatus === "error" ? "error" :
              (syncStatus === "done" || syncStatus === "finished") ? "idle" : "streaming";
            const errorText = syncStatus === "error" ? (event.error ?? null) : null;

            // Single writeDraft — message + connectionPhase + error fields are atomic (fixes bugs b and c)
            session.writeDraft((agentDraft: AgentState) => {
              if (receivedSessionId && agentDraft.activeSessionId && receivedSessionId !== agentDraft.activeSessionId) return;

              const currentSessionId = agentDraft.activeSessionId;
              const isSameSession = receivedSessionId && currentSessionId === receivedSessionId;

              // Only skip the live-mode switch when we're in "history" mode for the same session.
              // For "new-chat" mode we always need to switch, even if session IDs match.
              const isHistoryMode = agentDraft.messageSource.mode === "history";
              if (agentDraft.messageSource.mode !== "live" && !(isHistoryMode && isSameSession)) {
                (agentDraft as any).messageSource = { mode: "live", messages: [] };
              }

              const liveMessages = (agentDraft.messageSource as any).messages as any[];
              const existingIdx = liveMessages.findIndex((m: any) => m.id === session.buffer.assistantId);

              if (existingIdx >= 0) {
                const existingMsg = liveMessages[existingIdx];
                const localTextLen = (existingMsg.parts as MessagePart[])
                  .filter((p: MessagePart): p is TextPart => p.type === "text")
                  .reduce((acc: number, p: TextPart) => acc + (p.text?.length ?? 0), 0);
                const syncTextLen = syncParts
                  .filter((p: MessagePart): p is TextPart => p.type === "text")
                  .reduce((acc: number, p: TextPart) => acc + (p.text?.length ?? 0), 0);
                if (syncTextLen > localTextLen || Math.abs(syncTextLen - localTextLen) > 50) {
                  // H2: syncParts carries only text/reasoning — preserve tool, approval, file parts
                  const preserved = (existingMsg.parts as MessagePart[]).filter(
                    (p: MessagePart) => p.type !== "text" && p.type !== "reasoning"
                  );
                  existingMsg.parts = [...syncParts, ...preserved];
                }
                if (existingMsg.status !== "complete") {
                  existingMsg.status = (syncStatus === "done" || syncStatus === "finished") ? "complete" : "streaming";
                }
              } else {
                liveMessages.push({
                  id: session.buffer.assistantId,
                  role: "assistant",
                  parts: syncParts,
                  createdAt: session.buffer.assistantCreatedAt,
                  agentId: session.buffer.currentRespondingAgent ?? undefined,
                  status: (syncStatus === "done" || syncStatus === "finished") ? "complete" : "streaming",
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

    if (!session.signal.aborted) {
      // Flush any remaining buffer content
      if (session.buffer.snapshot().length > 0) session.commit();

      const agentState = callbacks.getAgentState(agent);
      const isError = agentState?.connectionPhase === "error";
      const isIdle = agentState?.connectionPhase === "idle";
      const effectiveSessionId = receivedSessionId ?? agentState?.activeSessionId;

      if (!isError && !isIdle && !receivedFinishEvent && effectiveSessionId) {
        callbacks.onReconnectNeeded(effectiveSessionId, reconnectAttempt);
        return;
      }

      if (!isError) {
        session.write({ connectionPhase: "idle", connectionError: null, reconnectAttempt: 0 });
      }
      callbacks.onStreamDone?.();
    } else {
      // Abort case: commit any partial parts (commit() drops silently if not current)
      session.commit("streaming");
    }
  }

  // Post-finally: switch to history mode and invalidate React Query caches.
  // This block is UNCHANGED — session.write() is not affected by this refactor.
  if (!session.signal.aborted) {
    if (receivedSessionId) {
      callbacks.onSessionId(receivedSessionId);
    }
    queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
    const completedSessionId = receivedSessionId ?? callbacks.getAgentState(agent)?.activeSessionId;
    if (completedSessionId) {
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(completedSessionId) });
      session.write({ messageSource: { mode: "history", sessionId: completedSessionId } });
    }
  }
}
