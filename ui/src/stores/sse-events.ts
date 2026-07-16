// ui/src/stores/sse-events.ts
//
// Hand-coded thin wrapper. Re-exports types from auto-generated
// sse.generated.ts. Single source of truth: crates/opex-types/src/sse.rs
// (registered for codegen in crates/opex-core/src/dto_export/sse_ts.rs).

export type {
  SseEvent,
  RichCardData,
  TableCard,
  MetricCard,
  MetricTrend,
  DataSessionIdPayload,
  SyncStatus,
  UsagePayload,
  ApprovalAction,
} from "@/types/sse.generated";

import type { SseEvent } from "@/types/sse.generated";

// NOTE: `reconnecting` event is server-AND-client emitted with the same
// shape (server: LLM-retry; client: SSE-reconnect). Both go through the
// generated SseEvent type — no separate ClientSseEvent type needed.

/**
 * Parse and validate a single SSE data payload.
 * Returns null for invalid JSON or missing `type` field.
 *
 * Codegen guarantees server output shape, so per-variant runtime
 * validation is no longer needed. If a Pi regression surfaces during
 * deploy, switch back to per-variant validation by re-extracting from
 * git history (pre-S6.5 sse-events.ts).
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
  if (typeof e.type !== "string") return null;
  return obj as SseEvent;
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

import { parseContentParts as parseParts, type ParsedContentPart } from "@/lib/message-parser";

/** Extract <think> blocks into reasoning parts and clean text parts from raw content */
export function parseContentParts(raw: string): ParsedContentPart[] {
  return parseParts(raw);
}
