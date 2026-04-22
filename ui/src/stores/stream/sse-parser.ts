// ui/src/stores/stream/sse-parser.ts
// Pure SSE parsing functions — no store, no React, no fetch.
// Extracted from ui/src/stores/sse-events.ts for colocation with
// the stream/ layer (Task 4.1).

import type { SseEvent } from "../sse-events";

/**
 * Parse a single SSE data line (JSON payload after `data: `) into an
 * SseEvent. Returns null if the payload is unparseable or
 * unrecognized. Pure — no side effects.
 */
export function parseSseEvent(raw: string): SseEvent | null {
  let obj: unknown;
  try {
    obj = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!obj || typeof obj !== "object") return null;
  const e = obj as Record<string, unknown>;
  const type = e.type;
  if (typeof type !== "string") return null;

  switch (type) {
    case "data-session-id": {
      const data = e.data as Record<string, unknown> | undefined;
      if (!data || typeof data.sessionId !== "string") return null;
      return { type, data: { sessionId: data.sessionId } };
    }
    case "start":
      return { type, messageId: typeof e.messageId === "string" ? e.messageId : undefined, agentName: typeof e.agentName === "string" ? e.agentName : undefined };
    case "text-start":
      return { type, id: typeof e.id === "string" ? e.id : undefined, agentName: typeof e.agentName === "string" ? e.agentName : undefined };
    case "text-delta":
      return { type, delta: typeof e.delta === "string" ? e.delta : "" };
    case "text-end":
      return { type };
    case "tool-input-start":
      if (typeof e.toolCallId !== "string" || typeof e.toolName !== "string") return null;
      return { type, toolCallId: e.toolCallId, toolName: e.toolName, agentName: typeof e.agentName === "string" ? e.agentName : undefined };
    case "tool-input-delta":
      if (typeof e.toolCallId !== "string") return null;
      return { type, toolCallId: e.toolCallId, inputTextDelta: typeof e.inputTextDelta === "string" ? e.inputTextDelta : "" };
    case "tool-input-available":
      if (typeof e.toolCallId !== "string") return null;
      return { type, toolCallId: e.toolCallId, input: e.input ?? {} };
    case "tool-output-available":
      if (typeof e.toolCallId !== "string") return null;
      return { type, toolCallId: e.toolCallId, output: e.output };
    case "file":
      if (typeof e.url !== "string") return null;
      return { type, url: e.url, mediaType: typeof e.mediaType === "string" ? e.mediaType : undefined };
    case "rich-card":
      return { type, cardType: typeof e.cardType === "string" ? e.cardType : "unknown", data: (e.data as Record<string, unknown>) ?? {} };
    case "sync":
      return {
        type,
        content: typeof e.content === "string" ? e.content : "",
        toolCalls: Array.isArray(e.toolCalls) ? e.toolCalls : [],
        status: typeof e.status === "string" ? e.status : "unknown",
        error: typeof e.error === "string" ? e.error : undefined,
      };
    case "step-start":
      if (typeof e.stepId !== "string") return null;
      return { type, stepId: e.stepId };
    case "step-finish":
      if (typeof e.stepId !== "string") return null;
      return { type, stepId: e.stepId, finishReason: typeof e.finishReason === "string" ? e.finishReason : "unknown" };
    case "finish":
      return {
        type,
        agentName: typeof e.agentName === "string" ? e.agentName : undefined,
      };
    case "error":
      return { type, errorText: typeof e.errorText === "string" ? e.errorText : "Unknown error" };
    case "reconnecting":
      return {
        type,
        attempt: typeof e.attempt === "number" ? e.attempt : 1,
        delay_ms: typeof e.delay_ms === "number" ? e.delay_ms : 2000,
      };
    case "tool-approval-needed": {
      if (typeof e.approvalId !== "string" || typeof e.toolName !== "string") return null;
      return {
        type,
        approvalId: e.approvalId,
        toolName: e.toolName,
        toolInput: (e.toolInput as Record<string, unknown>) ?? {},
        timeoutMs: typeof e.timeoutMs === "number" ? e.timeoutMs : 300000,
      };
    }
    case "tool-approval-resolved": {
      if (typeof e.approvalId !== "string") return null;
      const action = e.action as string;
      if (action !== "approved" && action !== "rejected" && action !== "timeout_rejected") return null;
      return {
        type,
        approvalId: e.approvalId,
        action,
        modifiedInput: e.modifiedInput != null ? (e.modifiedInput as Record<string, unknown>) : undefined,
      };
    }
    default:
      return null;
  }
}

/**
 * Splits an incoming chunk into complete SSE lines, buffering incomplete ones.
 * The `buffer.current` field accumulates any trailing partial line across calls.
 * Pure except for mutating `buffer.current` — callers own the buffer's lifetime.
 */
export function parseSSELines(chunk: string, buffer: { current: string }): string[] {
  buffer.current += chunk;
  const lines: string[] = [];
  let idx: number;
  while ((idx = buffer.current.indexOf("\n")) !== -1) {
    lines.push(buffer.current.slice(0, idx).replace(/\r$/, ""));
    buffer.current = buffer.current.slice(idx + 1);
  }
  return lines;
}
