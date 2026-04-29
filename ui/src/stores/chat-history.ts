import type { ChatMessage, MessagePart } from "./chat-types";
import type { MessageRow } from "@/types/api";
import { parseContentParts } from "@/stores/sse-events";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";

// ── Tool ordering helper ─────────────────────────────────────────────────────

/**
 * Sort consecutive runs of tool parts back into their declared order.
 * Parallel tool calls complete (and are saved to DB) in execution order, not
 * declaration order — so search_web_fresh may appear before agents_list even
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
export function convertHistory(rows: MessageRow[], isAgentStreaming?: boolean, selectedBranches?: Record<string, string>): ChatMessage[] {
  // Drop streaming placeholder rows FIRST — they are artifacts of incomplete
  // SSE flushes and historically have been INSERTed with parent_message_id=NULL,
  // which would confuse resolveActivePath into picking the placeholder as
  // roots[0] and shadowing the real conversation tree (Bug 2, 2026-04-20).
  // The authoritative assistant row is always saved separately on Finish.
  const nonStreamingRows = rows.filter(m => m.status !== "streaming");
  // Only walk resolveActivePath if branching data is actually present — saves
  // work on trunk-only conversations.
  const resolvedRows = selectedBranches && nonStreamingRows.some(r => r.parent_message_id != null)
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
  const toolCallMap = new Map<string, { name: string; arguments: unknown }>();
  const toolCallOrderMap = new Map<string, number>();
  for (const m of filtered) {
    if (m.role === "assistant" && m.tool_calls) {
      const calls = m.tool_calls as Array<{ id: string; name: string; arguments?: unknown }>;
      if (Array.isArray(calls)) {
        calls.forEach((tc, idx) => {
          if (tc.id) {
            toolCallMap.set(tc.id, { name: tc.name || "tool", arguments: tc.arguments ?? {} });
            toolCallOrderMap.set(tc.id, idx);
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
        parts: [{ type: "text", text: m.content || "" }],
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

      // Merge intermediate assistant messages (those with tool_calls) into one block
      // so tools don't stack as separate ARTY bubbles. The final message (no tool_calls)
      // always starts a new block for the text response.
      if (hasToolCalls && lastAssistantMsg && lastAssistantMsg.agentId === assistantAgentId) {
        if (newParts.length > 0) lastAssistantMsg.parts.push(...newParts);
        continue; // tool rows will attach to lastAssistantMsg
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

  // Final pass: filter empty messages, then sort tool parts within each consecutive
  // group back into declared order (parallel tool calls complete out of order in DB).
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
  const cached = queryClient.getQueryData<{ messages: MessageRow[] }>(qk.sessionMessages(sessionId));
  return cached ? convertHistory(cached.messages, false, selectedBranches) : [];
}

/** Get all raw MessageRow[] from React Query cache for a session (for sibling discovery). */
export function getCachedRawMessages(sessionId: string | null): MessageRow[] {
  if (!sessionId) return [];
  const cached = queryClient.getQueryData<{ messages: MessageRow[] }>(qk.sessionMessages(sessionId));
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
  const hasBranching = rows.some(r => r.parent_message_id != null);
  if (!hasBranching) {
    return [...rows].sort((a, b) => new Date(a.created_at).getTime() - new Date(b.created_at).getTime());
  }

  const childrenOf = new Map<string, MessageRow[]>();
  const roots: MessageRow[] = [];

  for (const r of rows) {
    if (r.parent_message_id == null) {
      roots.push(r);
    } else {
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

    const selectedId: string | undefined = selectedBranches[current.id];
    current = selectedId
      ? children.find(c => c.id === selectedId) ?? children[children.length - 1]
      : children[children.length - 1];
  }

  return path;
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
