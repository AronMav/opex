"use client";

import React, { memo, useState, type ReactNode } from "react";
import { useAutoAnimate } from "@formkit/auto-animate/react";
import { useChatStore } from "@/stores/chat-store";
import {
  selectCurrentActiveSessionId,
  selectCurrentAgent,
  selectCurrentPhaseIsActive,
  useChatActions,
  useShallow,
} from "@/stores/chat-selectors";
import { useAuthStore } from "@/stores/auth-store";
import { usePaletteStore } from "@/stores/palette-store";
import { useTranslation } from "@/hooks/use-translation";
import type { ChatMessage, MessagePart, TextPart as TextPartType } from "@/stores/chat-store";
import { findSiblings, getCachedRawMessages } from "@/stores/chat-store";
import { formatMessageTime } from "@/lib/format";
import { BranchNavigator } from "./BranchNavigator";
import { cn } from "@/lib/utils";
import { AlertCircle } from "lucide-react";
// Collapsible removed — tool grouping disabled
import { CompressionDivider } from "@/components/chat/CompressionDivider";
import { PartSkeleton } from "@/components/ui/loader";
import { Button } from "@/components/ui/button";
import { MessageActions } from "./MessageActions";
import { MessageEditForm } from "./MessageEditForm";
import { TextPart } from "./parts/TextPart";
import { ReasoningPart } from "./parts/ReasoningPart";
import { ToolCallPartView } from "@/components/chat/ToolCallPartView";
import { FileDataPartView } from "@/components/chat/FileDataPartView";
import {
  RoleAvatar,
  RichCardDataPartView,
} from "./avatar/RoleAvatar";
import { ApprovalCard } from "@/components/chat/ApprovalCard";
import { ClarifyCard } from "@/components/chat/ClarifyCard";
import { abortReasonLabel } from "@/components/chat/abort-reason-label";
import { useSwipeGesture } from "@/hooks/use-swipe-gesture";


// ── Parts render cache (PERF-03) ───────────────────────────────────────────
// Module-scope WeakMap: keys are ChatMessage object references.
// With PERF-02 in-place Immer mutation, only the currently-streaming message
// gets its parts updated — all other messages keep stable object references,
// so cache entries survive across renders. Entries are GC'd when ChatMessage
// objects leave scope.
const _partsRenderCache = new WeakMap<ChatMessage, ReactNode[]>();

// ── Tool grouping threshold ─────────────────────────────────────────────────
// Tool grouping removed — each tool call rendered individually.

// ── Tool status mapping ─────────────────────────────────────────────────────

import { mapToolPartState } from "@/lib/tool-state";

// ── Jump-to-message highlight ────────────────────────────────────────────────
// Flash class applied to the row currently targeted by the search palette /
// bookmark jump (useScrollToMessage sets highlightedMessageId for ~2s). The
// selector returns a boolean for THIS message so only the matched row
// re-renders when the highlight moves (Zustand strict-equal gating). `duration`
// tokens keep the fade design-system-compliant.
const HIGHLIGHT_CLASS = "ring-2 ring-primary/40 rounded-lg transition-opacity duration-1000";

function useIsHighlighted(message: ChatMessage): boolean {
  return usePaletteStore(
    (s) =>
      s.highlightedMessageId != null &&
      (s.highlightedMessageId === message.id ||
        !!message.mergedIds?.includes(s.highlightedMessageId)),
  );
}

// ── Empty part view (loading indicator for empty assistant messages) ─────────

function EmptyPartView() {
  return <PartSkeleton />;
}

// ── Part renderer dispatch ──────────────────────────────────────────────────

function renderPart(part: MessagePart, index: number, streaming = false) {
  switch (part.type) {
    case "text":
      return <TextPart key={`text-${index}`} text={part.text} streaming={streaming} />;
    case "reasoning":
      return <ReasoningPart key={`reasoning-${index}`} text={part.text} streaming={streaming} />;
    case "tool": {
      // The `file_handler(list)` call only exists to emit the interactive menu
      // card (rendered as a `rich-card` part) — its tool chip is pure noise that
      // lingers after the user picks an option. Suppress it; the menu card is the
      // visible affordance. Other file_handler actions (run) still show a chip.
      if (part.toolName === "file_handler" && (part.input as { action?: string })?.action === "list") {
        return null;
      }
      return (
        <ToolCallPartView
          key={`tool-${part.toolCallId}`}
          toolName={part.toolName}
          args={part.input}
          result={part.output}
          status={{ type: mapToolPartState(part.state) }}
        />
      );
    }
    case "file":
      return <FileDataPartView key={`file-${part.url}`} data={{ url: part.url, mediaType: part.mediaType }} />;
    case "rich-card":
      // Skip agent-turn rich cards — AgentTransitionDivider in MessageList replaces this
      if (part.cardType === "agent-turn") return null;
      return <RichCardDataPartView key={`card-${part.cardType}-${index}`} data={{ cardType: part.cardType, ...part.data }} />;
    case "approval":
      return <ApprovalCard key={`approval-${part.approvalId}`} part={part} />;
    case "clarify":
      return <ClarifyCard key={`clarify-${part.clarifyId}`} part={part} />;
    case "compression-divider":
      return (
        <CompressionDivider
          key={`compression-divider-${part.segmentIndex}`}
          segmentIndex={part.segmentIndex}
          totalSegments={part.totalSegments}
        />
      );
    default:
      return null;
  }
}

// ── Parts rendering (no grouping — each part rendered individually) ────────

function renderAllParts(parts: MessagePart[], streaming = false) {
  return parts
    .filter(p => !(p.type === "text" && p.text.trim().length === 0))
    .map((part, i) => renderPart(part, i, streaming));
}

// ── User message ────────────────────────────────────────────────────────────

function UserMessage({ message, sessionChannel, sessionUserId }: { message: ChatMessage; sessionChannel?: string; sessionUserId?: string }) {
  const { t, locale } = useTranslation();
  // REF-05: useShallow-gated read of the agentIcons record — shallow-equal
  // means this component won't re-render when an unrelated key is mutated.
  const agentIcons = useAuthStore(useShallow((s: { agentIcons: Record<string, string | null> }) => s.agentIcons));
  // REF-05: typed selector from chat-selectors (primitive — Zustand's default
  // strict equality is sufficient, no useShallow wrapper needed).
  const activeSessionId = useChatStore(selectCurrentActiveSessionId);
  const currentAgent = useChatStore(selectCurrentAgent);
  // Fix L: gate branch navigation while a turn is active for this session.
  // Switching an earlier branch mid-turn re-walks resolveActivePath to a
  // different trunk, so the live overlay (old branch's lineage) would render
  // after a different branch's history — two branches blended.
  const branchNavDisabled = useChatStore(selectCurrentPhaseIsActive);
  // REF-05: actions come from the store via a useShallow-gated bundle — stable
  // references replace the old `useChatStore.getState()` imperative access.
  const { regenerate } = useChatActions();

  // Compute branch siblings for this user message (only when branching data exists).
  // Fix M: thread the current agent so getCachedRawMessages picks THIS agent's
  // cache entry in a multi-agent shared session (not results[0] = other agent).
  const branchInfo = React.useMemo(() => {
    if (!message.parentMessageId || !activeSessionId) return null;
    const allRows = getCachedRawMessages(activeSessionId, currentAgent);
    if (allRows.length === 0) return null;
    const { siblings, index } = findSiblings(allRows, message.id);
    if (siblings.length <= 1) return null;
    return { parentMessageId: message.parentMessageId, siblings, index };
  }, [message.id, message.parentMessageId, activeSessionId, currentAgent]);

  const isReadOnly = sessionChannel === "heartbeat" || sessionChannel === "cron" || sessionChannel === "inter-agent";

  // Per-message agent sender via agentId prop (for inter-agent turn loop messages)
  const senderAgentName = message.agentId
    || (isReadOnly && sessionUserId?.startsWith("agent:") ? sessionUserId.slice(6) : null);
  const isAgentSender = !!senderAgentName;
  const senderIconUrl = senderAgentName ? agentIcons[senderAgentName] || undefined : undefined;

  const isSending = message.status === "sending";
  const isFailed = message.status === "failed";
  const isHighlighted = useIsHighlighted(message);
  const [editing, setEditing] = useState(false);

  // Swipe right to edit (mobile)
  const swipeHandlers = useSwipeGesture({
    onSwipeRight: () => {
      // Trigger edit via the message actions - find and click the edit button
      const el = document.querySelector(`[data-msg-id="${message.id}"] [data-action="edit"]`) as HTMLButtonElement | null;
      el?.click();
    },
    threshold: 80,
  });

  return (
    <div
      data-role={isAgentSender ? "agent-sender" : "user"}
      data-msg-id={message.id}
      {...swipeHandlers}
      className={cn(
        "group flex gap-3 py-5 md:py-6 border-t border-border/30 dark:border-border/30 first:border-t-0",
        isAgentSender && "bg-muted/20 dark:bg-muted/10 rounded-lg px-3",
        isFailed && "border-l-2 border-l-destructive pl-3",
        isHighlighted && HIGHLIGHT_CLASS
      )}
    >
      <span className="message-avatar">
        <RoleAvatar
          role={isAgentSender ? "agent-sender" : "user"}
          iconUrl={isAgentSender ? senderIconUrl : undefined}
          agentName={isAgentSender ? senderAgentName : undefined}
        />
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-2">
        <div className="message-header flex items-center justify-between min-h-5 gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <span className={`text-xs font-semibold uppercase tracking-wider truncate max-w-30 ${isAgentSender ? "text-muted-foreground-subtle" : "text-primary"}`}>
              {isAgentSender ? senderAgentName : t("chat.you")}
            </span>
            {message.createdAt && (
              <span className="text-3xs font-mono tabular-nums text-muted-foreground-subtle md:opacity-0 md:group-hover:opacity-100 md:group-focus-within:opacity-100 transition-opacity shrink-0">
                {formatMessageTime(message.createdAt, locale)}
              </span>
            )}
          </div>
          {!editing && (
            <div className="flex shrink-0 items-center gap-1">
              {branchInfo && (
                <BranchNavigator
                  parentMessageId={branchInfo.parentMessageId}
                  siblings={branchInfo.siblings}
                  currentIndex={branchInfo.index}
                  disabled={branchNavDisabled}
                />
              )}
              <MessageActions message={message} showReload={false} onEdit={() => setEditing(true)} />
            </div>
          )}
        </div>
        {editing ? (
          <MessageEditForm
            initialText={message.parts
              .filter((p): p is TextPartType => p.type === "text")
              .map((p) => p.text)
              .join("\n")}
            onSubmit={(text) => {
              setEditing(false);
              useChatStore.getState().forkAndRegenerate(message.id, text);
            }}
            onCancel={() => setEditing(false)}
          />
        ) : (
          <div className={cn("min-w-0 space-y-3", isSending && "opacity-70")}>
            {message.parts.map((part, i) => renderPart(part, i))}
          </div>
        )}
        {isFailed && (
          <div className="flex items-center gap-2 mt-1 text-xs text-destructive">
            <AlertCircle className="h-4 w-4 shrink-0" />
            <span>{t("chat.failedToSend")}</span>
            <Button
              type="button"
              variant="link"
              size="sm"
              className="underline hover:no-underline h-auto p-0 text-destructive"
              onClick={() => regenerate()}
            >
              {t("chat.retry")}
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}

// ── Assistant message ───────────────────────────────────────────────────────

function AssistantMessage({ message, continuesPrevious = false }: { message: ChatMessage; continuesPrevious?: boolean }) {
  const { t, locale } = useTranslation();
  // REF-05: typed selector from chat-selectors (primitive value — Zustand's
  // default strict-equality gating is enough, no useShallow wrapper needed).
  const currentAgent = useChatStore(selectCurrentAgent);
  // REF-05: useShallow-gated read of the agentIcons record.
  const agentIcons = useAuthStore(useShallow((s: { agentIcons: Record<string, string | null> }) => s.agentIcons));

  // Direct agentId from message props -- no more AgentTurnCounterContext hack
  const agentName = message.agentId || currentAgent;
  const agentIconUrl = agentName ? agentIcons[agentName] || null : null;

  const hasParts = message.parts.length > 0;
  const isHighlighted = useIsHighlighted(message);

  // PERF-03: WeakMap cache for rendered parts — only re-render if message object changed.
  // Cache key is the ChatMessage object reference; PERF-02 in-place mutation ensures
  // non-streaming messages keep stable refs so they get cache hits across re-renders.
  const [animateRef] = useAutoAnimate({ duration: 200 });

  // T16.5: testid for E2E — present on any assistant message that is not
  // actively text-streaming. Live streaming messages carry status === "streaming"
  // (see stream-processor.ts); history + finalized messages have status of
  // undefined / "complete" / "aborted" / "failed". Also gates ReasoningPart's
  // auto-expand + pulse: streaming messages get a fresh object ref each tick
  // (cache miss → re-render with streaming=true), the finalized ref caches the
  // collapsed reasoning view.
  const isComplete = (message.status as string | undefined) !== "streaming";

  let renderedParts: ReactNode[] | undefined;
  if (hasParts) {
    renderedParts = _partsRenderCache.get(message);
    if (!renderedParts) {
      renderedParts = renderAllParts(message.parts, !isComplete);
      _partsRenderCache.set(message, renderedParts);
    }
  }

  // Swipe left to regenerate (mobile)
  const { regenerate } = useChatActions();
  const swipeHandlers = useSwipeGesture({
    onSwipeLeft: () => {
      if (isComplete) regenerate();
    },
    threshold: 80,
  });

  return (
    <div
      data-role="assistant"
      data-msg-id={message.id}
      data-testid={isComplete ? "message-complete" : undefined}
      {...swipeHandlers}
      className={cn(
        "group flex gap-3",
        continuesPrevious
          ? "pt-0 pb-2 md:pb-3"
          : "py-5 md:py-6 border-t border-border/30 dark:border-border/30 first:border-t-0",
        isHighlighted && HIGHLIGHT_CLASS,
      )}
    >
      <span className="message-avatar">
        {continuesPrevious ? (
          <span className="block h-9 w-9" aria-hidden />
        ) : (
          <RoleAvatar role="assistant" iconUrl={agentIconUrl} agentName={agentName} />
        )}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-2">
        {!continuesPrevious && (
          <div className="message-header flex items-center justify-between min-h-5">
            <div className="flex min-w-0 items-center gap-2">
              <span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground-subtle truncate max-w-30">
                {agentName || t("chat.assistant")}
              </span>
              {message.createdAt && (
                <span className="text-3xs font-mono tabular-nums text-muted-foreground-subtle md:opacity-0 md:group-hover:opacity-100 md:group-focus-within:opacity-100 transition-opacity">
                  {formatMessageTime(message.createdAt, locale)}
                </span>
              )}
              {message.isMirror && (
                <span className="text-3xs text-cron ml-1">↩ cron</span>
              )}
            </div>
            <MessageActions message={message} showReload />
          </div>
        )}
        <div ref={animateRef} className="min-w-0 space-y-3">
          {hasParts ? renderedParts : <EmptyPartView />}
        </div>
        {message.status === "aborted" && (
          <p className="mt-2 text-xs italic text-muted-foreground">
            {abortReasonLabel(message.abortReason, t)}
          </p>
        )}
      </div>
    </div>
  );
}

// ── Main MessageItem ────────────────────────────────────────────────────────

// REF-05: React.memo with default shallow prop comparison — unrelated parent
// re-renders no longer force a full re-render of every row in the list.
function MessageItemImpl({
  message,
  sessionChannel,
  sessionUserId,
  continuesPrevious = false,
}: {
  message: ChatMessage;
  sessionChannel?: string;
  sessionUserId?: string;
  /**
   * True when this assistant ChatMessage is a continuation of the previous
   * one — same agent, no user message in between (typically the next
   * tool-loop iteration of the same turn after Phase 1's per-iteration
   * UUIDs split a turn into multiple ChatMessages). Renderer hides the
   * avatar + header so the bubble visually flows from the previous one.
   */
  continuesPrevious?: boolean;
}) {
  if (message.role === "user") {
    return <UserMessage message={message} sessionChannel={sessionChannel} sessionUserId={sessionUserId} />;
  }
  return <AssistantMessage message={message} continuesPrevious={continuesPrevious} />;
}

export const MessageItem = memo(MessageItemImpl);
MessageItem.displayName = "MessageItem";
