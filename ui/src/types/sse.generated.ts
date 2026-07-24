// @generated — do not edit by hand.
// Source of truth: types annotated with #[ts(export)] in crates/opex-core/.
// Regenerate with: make gen-types

export type ApprovalAction = "approved" | "rejected" | "timeout_rejected";

export type DataSessionIdPayload = { sessionId: string, contextLimit: number | null, };

export type MetricCard = { title: string | null, value: string | null, label: string | null, trend: MetricTrend | null, };

export type MetricTrend = "up" | "down" | "flat";

export type RichCardData = { "cardType": "table", "data": TableCard } | { "cardType": "metric", "data": MetricCard } | { "cardType": "other", "data": { cardType: string, data: unknown, } };

export type SseEvent = { "type": "data-session-id", data: DataSessionIdPayload, transient: boolean, } | { "type": "start", messageId: string, agentName: string, } | { "type": "step-start", stepId: string, messageId: string, agentName: string, } | { "type": "text-start", id: string, agentName: string, } | { "type": "text-delta", id: string, delta: string, } | { "type": "text-end", id: string, } | { "type": "tool-input-start", toolCallId: string, toolName: string, agentName: string, parallelBatchId: string | null, } | { "type": "tool-input-delta", toolCallId: string, inputTextDelta: string, } | { "type": "tool-input-available", toolCallId: string, toolName: string, input: unknown, parallelBatchId: string | null, } | { "type": "tool-output-available", toolCallId: string, output: string, } | { "type": "file", url: string, mediaType: string, 
/**
 * Optional display name (e.g. "image.png"); omitted when unknown.
 */
filename: string | null, } | { "type": "rich-card" } & RichCardData | { "type": "clarify-needed", clarifyId: string, question: string, choices: Array<string>, timeoutMs: number, } | { "type": "tool-approval-needed", approvalId: string, toolName: string, toolInput: unknown, 
/**
 * u64 to match StreamEvent. Rendered as `number` in TS via override.
 */
timeoutMs: number, } | { "type": "tool-approval-resolved", approvalId: string, action: ApprovalAction, modifiedInput: unknown | null, } | { "type": "finish", agentName: string, } | { "type": "error", errorText: string, } | { "type": "reconnecting", attempt: number, delay_ms: number, } | { "type": "sync", content: string, toolCalls: Array<unknown>, status: SyncStatus, error: string | null, } | { "type": "sync_begin", boundaryMessageId: string | null, runStatus: SyncStatus, 
/**
 * Буфер переполнился И компакция (слияние соседних text-delta одного
 * блока) не смогла решительно ужать его (ниже половины ёмкости) —
 * некомпактируемый или слабо-компактируемый флуд. Replay неполон;
 * клиент показывает баннер и полагается на финальный рефетч истории.
 */
truncated: boolean, } | { "type": "sync_end", lastSeq: number | null, } | { "type": "usage" } & UsagePayload;

export type SyncStatus = "finished" | "error" | "interrupted" | "running";

export type TableCard = { title: string | null, columns: Array<string>, 
/**
 * Each cell is mixed string|number per existing UI consumer.
 */
rows: Array<Array<string | number>>, };

export type UsagePayload = { inputTokens: number, outputTokens: number, 
/**
 * Required — converter ALWAYS emits the currently-responding agent.
 */
agentName: string, cacheReadTokens: number | null, cacheCreationTokens: number | null, reasoningTokens: number | null, };
