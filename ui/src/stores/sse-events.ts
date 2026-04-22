// Discriminated union of all SSE events from the backend.
// Keep in sync with enum StreamEvent in crates/hydeclaw-core/src/agent/engine.rs.

export interface AgentTurnCard {
  agentName: string;
  reason: string;
}

export type SseEvent =
  | { type: "data-session-id"; data: { sessionId: string } }
  | { type: "start"; messageId?: string; agentName?: string }
  | { type: "text-start"; id?: string; agentName?: string }
  | { type: "text-delta"; delta: string }
  | { type: "text-end" }
  | { type: "tool-input-start"; toolCallId: string; toolName: string; agentName?: string }
  | { type: "tool-input-delta"; toolCallId: string; inputTextDelta: string }
  | { type: "tool-input-available"; toolCallId: string; input: unknown }
  | { type: "tool-output-available"; toolCallId: string; output: unknown }
  | { type: "file"; url: string; mediaType?: string }
  | { type: "rich-card"; cardType: string; data: Record<string, unknown> }
  | { type: "sync"; content: string; toolCalls: unknown[]; status: string; error?: string }
  | { type: "step-start"; stepId: string }
  | { type: "step-finish"; stepId: string; finishReason: string }
  | { type: "tool-approval-needed"; approvalId: string; toolName: string; toolInput: Record<string, unknown>; timeoutMs: number }
  | { type: "tool-approval-resolved"; approvalId: string; action: "approved" | "rejected" | "timeout_rejected"; modifiedInput?: Record<string, unknown> }
  | { type: "finish"; agentName?: string }
  | { type: "error"; errorText: string }
  | { type: "reconnecting"; attempt: number; delay_ms: number };

/**
 * Parse and validate a single SSE data payload.
 * Returns null for invalid JSON, missing type, or unknown event type.
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
 * Extracted from chat-store.ts to be testable and reusable.
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

/**
 * Extract the event ID from an SSE "id:" line.
 * Returns null if not an id line or if the value is empty/whitespace.
 */
export function extractSseEventId(line: string): string | null {
  if (line.startsWith("id:")) {
    const val = line.slice(3).trim();
    return val || null;
  }
  return null;
}

import { parseContentParts as parseParts } from "@/lib/message-parser";

/** Extract <think> blocks into reasoning parts and clean text parts from raw content */
export function parseContentParts(raw: string): any[] {
  return parseParts(raw);
}
