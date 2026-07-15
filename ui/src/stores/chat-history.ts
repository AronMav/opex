import type { ChatMessage, MessagePart } from "./chat-types";
import type { MessageRow } from "@/types/api";
import { parseContentParts } from "@/stores/sse-events";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";

// ── User message part parser ─────────────────────────────────────────────────

const USER_IMAGE_RE = /\[User attached an image: ([^\]]+)\]/g;
const IMAGE_VISION_RE = /<vision>[\s\S]*?<\/vision>/g;
// Strips URL-fetch enrichment injected by enrich_message_text() for LLM context.
// Format: "[Content of URL <url>]:\n<<<EXTERNAL_CONTENT ...>>>...<END>>>".
// This content is LLM-only context — not what the user typed.
const URL_CONTENT_RE = /\[Content of URL [^\]]+\][\s\S]*/g;

function guessImageMime(url: string): string {
  const ext = url.split("?")[0].split(".").pop()?.toLowerCase() ?? "";
  if (ext === "png") return "image/png";
  if (ext === "gif") return "image/gif";
  if (ext === "webp") return "image/webp";
  if (ext === "bmp") return "image/bmp";
  return "image/jpeg";
}

/**
 * Convert stored user message content to MessageParts.
 * Extracts [User attached an image: URL] hints into FileParts so images
 * render as <img> when history loads — same behaviour as live streaming.
 * Strips [Image (vision): ...] hints — LLM context, not for display.
 */
function parseUserMessageParts(content: string): MessagePart[] {
  const parts: MessagePart[] = [];
  const imageUrls: string[] = [];
  let text = content;

  USER_IMAGE_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = USER_IMAGE_RE.exec(content)) !== null) {
    imageUrls.push(m[1]);
    text = text.replace(m[0], "");
  }

  text = text.replace(IMAGE_VISION_RE, "");
  text = text.replace(URL_CONTENT_RE, "");

  const trimmed = text.trim();
  if (trimmed) parts.push({ type: "text", text: trimmed });
  for (const url of imageUrls) {
    parts.push({ type: "file", url, mediaType: guessImageMime(url) });
  }

  if (parts.length === 0) parts.push({ type: "text", text: content });
  return parts;
}

// ── Tool ordering helper ─────────────────────────────────────────────────────

/**
 * Sort consecutive runs of tool parts back into their declared order.
 * Parallel tool calls complete (and are saved to DB) in execution order, not
 * declaration order — so search_web may appear before agents_list even
 * when agents_list was declared first in tool_calls[].
 * Text / file / rich-card parts are left in place as separators between groups.
 */
function sortToolGroups(parts: MessagePart[], orderMap: Map<string, number>): MessagePart[] {
  let changed = false;
  const result: MessagePart[] = [];
  let i = 0;
  while (i < parts.length) {
    if (parts[i].type !== "tool") { result.push(parts[i++]); continue; }
    const start = i;
    while (i < parts.length && parts[i].type === "tool") i++;
    const group = parts.slice(start, i);
    const sorted = [...group].sort((a, b) => {
      const ia = a.type === "tool" ? (orderMap.get(a.toolCallId) ?? Infinity) : Infinity;
      const ib = b.type === "tool" ? (orderMap.get(b.toolCallId) ?? Infinity) : Infinity;
      return ia - ib;
    });
    if (sorted.some((p, j) => p !== group[j])) changed = true;
    result.push(...sorted);
  }
  return changed ? result : parts;
}

// ── History conversion (MessageRow[] -> ChatMessage[]) ───────────────────────

/**
 * Converts flat database rows into structured ChatMessage objects.
 * Implements "Virtual Merging" (Stage 2): consecutive assistant/tool blocks
 * from the same agent are merged into a single visual message to ensure
 * stable tool grouping and consistent identity.
 */
// Referential cache (P1): converting a session's history is O(n) tree work that
// re-runs on every render even when the source rows haven't changed. Key the
// result by the exact `rows` array reference (React Query hands back the SAME
// array until a refetch, at which point the WeakMap entry is naturally dropped).
// The extra args (stream state, branch selection) participate in the key so a
// change never returns a stale conversion.
//
// A SINGLE rows array is legitimately converted with several arg-combos in one
// render pass (e.g. `getCachedHistoryMessages` with streaming=false plus a live
// render path with the current streaming flag), so we keep a SMALL ring of
// entries per rows array instead of one slot — a single slot would thrash to a
// 0% hit rate under those alternating callers.
const HISTORY_CACHE_ENTRIES = 8;
type HistoryCacheEntry = { streaming?: boolean; branches?: Record<string, string>; result: ChatMessage[] };
const historyCache = new WeakMap<MessageRow[], HistoryCacheEntry[]>();

export function convertHistory(rows: MessageRow[], isAgentStreaming?: boolean, selectedBranches?: Record<string, string>): ChatMessage[] {
  let entries = historyCache.get(rows);
  if (entries) {
    const hit = entries.find((e) => e.streaming === isAgentStreaming && e.branches === selectedBranches);
    if (hit) return hit.result;
  } else {
    entries = [];
    historyCache.set(rows, entries);
  }
  const result = convertHistoryImpl(rows, isAgentStreaming, selectedBranches);
  entries.push({ streaming: isAgentStreaming, branches: selectedBranches, result });
  if (entries.length > HISTORY_CACHE_ENTRIES) entries.shift();
  return result;
}

function convertHistoryImpl(rows: MessageRow[], isAgentStreaming?: boolean, selectedBranches?: Record<string, string>): ChatMessage[] {
  // Drop streaming placeholder rows FIRST — they are artifacts of incomplete
  // SSE flushes and historically have been INSERTed with parent_message_id=NULL,
  // which would confuse resolveActivePath into picking the placeholder as
  // roots[0] and shadowing the real conversation tree (Bug 2, 2026-04-20).
  // The authoritative assistant row is always saved separately on Finish.
  const nonStreamingRows = rows.filter(m => m.status !== "streaming");
  // Only walk resolveActivePath if branching data is actually present — saves
  // work on trunk-only conversations.
  // D1 (2026-05-13): real branching is signaled by branch_from_message_id
  // (m012 schema), not parent_message_id which exists on every non-root row.
  // Short-circuit trunk-only conversations to a no-walk path.
  const resolvedRows = selectedBranches && nonStreamingRows.some(r => r.branch_from_message_id != null)
    ? resolveActivePath(nonStreamingRows, selectedBranches)
    : nonStreamingRows;
  const filtered = resolvedRows;

  const messages: ChatMessage[] = [];
  let lastAssistantMsg: ChatMessage | null = null;
  let lastAgentId: string | undefined = undefined;

  // Tool call map for resolving tool names/inputs from the main assistant record,
  // and order map for sorting parallel tool results back into declared order.
  // Parallel tool calls complete out of order (fastest first), so DB insertion
  // timestamps don't match the declared order in tool_calls[].
  // Global index (not per-message) prevents mis-sorting when consecutive assistant
  // messages share a tool-result run because an intermediate message has no text.
  const toolCallMap = new Map<string, { name: string; arguments: unknown }>();
  const toolCallOrderMap = new Map<string, number>();
  let globalToolIdx = 0;
  for (const m of filtered) {
    if (m.role === "assistant" && m.tool_calls) {
      const calls = m.tool_calls as Array<{ id: string; name: string; arguments?: unknown }>;
      if (Array.isArray(calls)) {
        calls.forEach((tc) => {
          if (tc.id) {
            toolCallMap.set(tc.id, { name: tc.name || "tool", arguments: tc.arguments ?? {} });
            toolCallOrderMap.set(tc.id, globalToolIdx++);
          }
        });
      }
    }
  }

  for (const m of filtered) {
    if (m.role === "user") {
      // Finalize any pending assistant message before starting a user block
      if (lastAssistantMsg) {
        messages.push(lastAssistantMsg);
        lastAssistantMsg = null;
      }
      if (m.agent_id) lastAgentId = m.agent_id;
      messages.push({
        id: m.id,
        role: "user",
        parts: parseUserMessageParts(m.content || ""),
        createdAt: m.created_at,
        agentId: m.agent_id ?? undefined,
        parentMessageId: m.parent_message_id ?? undefined,
        branchFromMessageId: m.branch_from_message_id ?? undefined,
      });
    } else if (m.role === "assistant" && !m.tool_call_id) {
      // Assistant text block
      const assistantAgentId = m.agent_id ?? lastAgentId;
      if (m.agent_id) lastAgentId = m.agent_id;

      const newParts = parseContentParts(m.content || "");
      const hasToolCalls = Array.isArray(m.tool_calls) && (m.tool_calls as unknown[]).length > 0;

      // Merge intermediate iterations into one visual bubble (same UX as
      // before). Each merged row's id is tracked in mergedIds (a provenance
      // record of the DB rows folded into this bubble).
      if (hasToolCalls && lastAssistantMsg && lastAssistantMsg.agentId === assistantAgentId) {
        if (newParts.length > 0) lastAssistantMsg.parts.push(...newParts);
        (lastAssistantMsg.mergedIds = lastAssistantMsg.mergedIds ?? []).push(m.id);
        continue;
      }

      if (lastAssistantMsg) messages.push(lastAssistantMsg);
      // Map DB `status = 'aborted'` (+ stable `abort_reason`) to store fields
      // consumed by the AssistantMessage footer. Other DB statuses ("streaming",
      // "finished", ...) are not represented in ChatMessage.status; they're
      // either filtered above or treated as confirmed.
      const isAborted = m.status === "aborted";
      lastAssistantMsg = {
        id: m.id,
        role: "assistant",
        parts: newParts,
        createdAt: m.created_at,
        agentId: assistantAgentId,
        parentMessageId: m.parent_message_id ?? undefined,
        branchFromMessageId: m.branch_from_message_id ?? undefined,
        status: isAborted ? "aborted" : undefined,
        abortReason: isAborted ? (m.abort_reason ?? null) : undefined,
        isMirror: m.is_mirror ?? false,
      };
    } else if (m.role === "tool" && m.tool_call_id) {
      // Tool result block — always attach to the latest assistant message
      if (!lastAssistantMsg) continue; // Skip: preceding assistant used pre-built parts
      const tc = toolCallMap.get(m.tool_call_id);

      // Extract inline markers (__file__:, __rich_card__:)
      const lines = (m.content || "").split("\n");
      const cleanLines: string[] = [];
      for (const line of lines) {
        if (line.startsWith("__file__:")) {
          try {
            const meta = JSON.parse(line.slice("__file__:".length));
            if (meta.url) {
              lastAssistantMsg.parts.push({
                type: "file",
                url: meta.url,
                mediaType: meta.mediaType || "application/octet-stream",
              });
            }
          } catch { /* ignore */ }
        } else if (line.startsWith("__rich_card__:")) {
          try {
            const data = JSON.parse(line.slice("__rich_card__:".length));
            const cardType = data.card_type || data.cardType || "unknown";
            lastAssistantMsg.parts.push({
              type: "rich-card",
              cardType,
              data,
            });
          } catch { /* ignore */ }
        } else {
          cleanLines.push(line);
        }
      }

      lastAssistantMsg.parts.push({
        type: "tool",
        toolCallId: m.tool_call_id,
        toolName: tc?.name || "tool",
        state: "output-available",
        input: (tc?.arguments as Record<string, unknown>) ?? {},
        output: cleanLines.join("\n"),
      });
    }
  }

  if (lastAssistantMsg) messages.push(lastAssistantMsg);

  // Final pass: filter empty messages, sort parallel tool parts within each
  // step's tool-group (parallel tool calls complete in execution order, not
  // declaration order — sort restores declared order). Step boundaries
  // already separate iterations so cross-iteration sorting is not a concern.
  return messages.filter(m => m.parts.length > 0).map(m => {
    if (m.role !== "assistant") return m;
    const sorted = sortToolGroups(m.parts, toolCallOrderMap);
    return sorted === m.parts ? m : { ...m, parts: sorted };
  });
}

/**
 * Read-through cache peek — called from Zustand store actions where React hooks
 * are unavailable. Components access this data via useSessionMessages() hook.
 * See ARCH-02 audit (phase 34): queryClient.getQueryData is intentional here and
 * in sendMessage(); no React component calls getQueryData directly.
 */
export function getCachedHistoryMessages(sessionId: string | null, selectedBranches?: Record<string, string>): ChatMessage[] {
  if (!sessionId) return [];
  // useSessionMessages stores at a 4-element key [...qk.sessionMessages(id), agent].
  // getQueriesData with the 3-element prefix matches it regardless of the agent suffix.
  // A session belongs to exactly one agent, so results[0] is the only possible entry.
  const results = queryClient.getQueriesData<{ messages: MessageRow[] }>({ queryKey: qk.sessionMessages(sessionId) });
  const cached = results[0]?.[1];
  return cached ? convertHistory(cached.messages, false, selectedBranches) : [];
}

/** Get all raw MessageRow[] from React Query cache for a session (for sibling discovery). */
export function getCachedRawMessages(sessionId: string | null): MessageRow[] {
  if (!sessionId) return [];
  const results = queryClient.getQueriesData<{ messages: MessageRow[] }>({ queryKey: qk.sessionMessages(sessionId) });
  const cached = results[0]?.[1];
  return cached?.messages ?? [];
}

// ── Branch resolution ─────────────────────────────────────────────────────

/**
 * Given all messages (including all branches) and the user's branch selections,
 * returns the linear path of messages to display.
 */
export function resolveActivePath(
  rows: MessageRow[],
  selectedBranches: Record<string, string>,
): MessageRow[] {
  // D1 (2026-05-13): defense-in-depth — even if called directly, the walker
  // short-circuits when there are no forks. Without this fix it walked the
  // tree on every conversation because parent_message_id is non-null on
  // ~100% of rows.
  const hasBranching = rows.some(r => r.branch_from_message_id != null);
  if (!hasBranching) {
    return [...rows].sort((a, b) => new Date(a.created_at).getTime() - new Date(b.created_at).getTime());
  }

  const childrenOf = new Map<string, MessageRow[]>();
  const hasDescendants = new Set<string>();
  const roots: MessageRow[] = [];

  for (const r of rows) {
    if (r.parent_message_id == null) {
      roots.push(r);
    } else {
      hasDescendants.add(r.parent_message_id);
      const siblings = childrenOf.get(r.parent_message_id) ?? [];
      siblings.push(r);
      childrenOf.set(r.parent_message_id, siblings);
    }
  }

  for (const [, children] of childrenOf) {
    children.sort((a, b) => new Date(a.created_at).getTime() - new Date(b.created_at).getTime());
  }

  roots.sort((a, b) => new Date(a.created_at).getTime() - new Date(b.created_at).getTime());
  if (roots.length === 0) return [];

  const path: MessageRow[] = [];
  let current: MessageRow | undefined = roots[0];

  while (current) {
    path.push(current);
    const children = childrenOf.get(current.id);
    if (!children || children.length === 0) break;

    // Parallel tool batches: all results are siblings with the same parent.
    // D2 (2026-05-13): the heir of a parallel batch is the sibling whose id
    // is parent_message_id of a later message — backend's `chain_parent`
    // advances to declaration-last parallel tool, which is rarely the same
    // as created_at-last (parallel tools complete in execution-speed order).
    // Swap heir into the last slot before the existing path-push logic.
    // Work on a copy: childrenOf entries can be revisited via forks.
    if (children.length > 1 && children.every(c => c.role === "tool")) {
      const ordered = [...children];
      const heirIdx = ordered.findIndex(c => hasDescendants.has(c.id));
      if (heirIdx >= 0 && heirIdx !== ordered.length - 1) {
        const heir = ordered.splice(heirIdx, 1)[0];
        ordered.push(heir);
      }
      path.push(...ordered.slice(0, -1));
      current = ordered[ordered.length - 1];
      continue;
    }

    const selectedId: string | undefined = selectedBranches[current.id];
    current = selectedId
      ? children.find(c => c.id === selectedId) ?? children[children.length - 1]
      : children[children.length - 1];
  }

  return path;
}

// ── Message search ────────────────────────────────────────────────────────────

export interface SearchMatch {
  messageId: string;
  partIndex: number;
  ranges: { start: number; end: number }[];
}

/**
 * Search all text parts of the given messages for the query string.
 * Returns one SearchMatch per (message, partIndex) pair that contains at least one match.
 * - Case-insensitive.
 * - Uses indexOf in a loop — NOT RegExp (avoids escaping hazards on user input).
 * - Skips non-text parts (tool, file, rich-card, approval, step-group, reasoning).
 * - Empty query returns [].
 */
export function searchMessages(query: string, messages: ChatMessage[]): SearchMatch[] {
  if (!query) return [];
  const lower = query.toLowerCase();
  const results: SearchMatch[] = [];
  for (const msg of messages) {
    msg.parts.forEach((part, partIndex) => {
      if (part.type !== "text") return;
      const text = (part as { type: "text"; text: string }).text;
      const textLower = text.toLowerCase();
      const ranges: { start: number; end: number }[] = [];
      let pos = 0;
      while (true) {
        const idx = textLower.indexOf(lower, pos);
        if (idx === -1) break;
        ranges.push({ start: idx, end: idx + lower.length });
        pos = idx + lower.length;
      }
      if (ranges.length > 0) {
        results.push({ messageId: msg.id, partIndex, ranges });
      }
    });
  }
  return results;
}

/** Find all sibling messages (sharing the same parent, same role). */
export function findSiblings(rows: MessageRow[], messageId: string): { siblings: MessageRow[]; index: number } {
  const msg = rows.find(r => r.id === messageId);
  if (!msg || !msg.parent_message_id) return { siblings: msg ? [msg] : [], index: 0 };

  const siblings = rows
    .filter(r => r.parent_message_id === msg.parent_message_id && r.role === msg.role)
    .sort((a, b) => new Date(a.created_at).getTime() - new Date(b.created_at).getTime());

  return { siblings, index: siblings.findIndex(s => s.id === messageId) };
}
