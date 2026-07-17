"use client";

import React, { useCallback, useEffect, useMemo, useRef, type ReactNode } from "react";
import { Virtuoso } from "react-virtuoso";
import { useChatStore } from "@/stores/chat-store";
import type { ChatMessage } from "@/stores/chat-store";
import { Button } from "@/components/ui/button";
import { CometLoader, CircularLoader } from "@/components/ui/loader";

import { MessageItem } from "./MessageItem";
import { useChatAutoscroll } from "./use-chat-autoscroll";
import { useScrollMemoryWrite } from "./hooks/use-scroll-memory";
import { setVirtuosoHandle } from "./message-list-handle";
import { AgentTransitionDivider } from "@/components/chat/AgentTransitionDivider";
import { VirtuosoList, VirtuosoListItem } from "@/components/chat/virtuoso-list-roles";
import { useSessions } from "@/lib/queries";
import { ChevronDown } from "lucide-react";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/hooks/use-translation";

// ── Animation suppression ──────────────────────────────────────────────────

function isNewMessage(msg: ChatMessage): boolean {
  if (!msg.createdAt) return false;
  return Date.now() - new Date(msg.createdAt).getTime() < 2000;
}

// ── Loading skeletons ──────────────────────────────────────────────────────

export function MessageSkeleton() {
  return (
    <div className="flex gap-3 py-5 md:py-6">
      <div className="h-9 w-9 rounded-xl bg-muted/50 animate-pulse shrink-0" />
      <div className="flex-1 space-y-2">
        <div className="h-3 w-20 rounded bg-muted/50 animate-pulse" />
        <div className="h-4 w-full rounded bg-muted/30 animate-pulse" />
        <div className="h-4 w-3/4 rounded bg-muted/30 animate-pulse" />
      </div>
    </div>
  );
}

function MessageListSkeleton() {
  return (
    <div className="mx-auto w-full max-w-4xl px-3 md:px-6 space-y-2">
      {[1, 2, 3, 4].map((i) => (
        <MessageSkeleton key={i} />
      ))}
    </div>
  );
}

// ── Thinking indicator ──────────────────────────────────────────────────────

function ThinkingMessage() {
  const { t } = useTranslation();
  return (
    <div
      role="status"
      aria-live="polite"
      aria-label={t("chat.thinking")}
      data-testid="thinking-indicator"
      className="pt-1 pb-2 pl-12 animate-in fade-in slide-in-from-bottom-2 duration-300 ease-out"
    >
      <CometLoader />
    </div>
  );
}

// ── Scroll-to-bottom button ─────────────────────────────────────────────────

function ScrollToBottomButton({
  visible,
  isStreaming,
  newTokenCount,
  onClick,
  ariaLabel,
}: {
  visible: boolean;
  isStreaming: boolean;
  newTokenCount: number;
  onClick: () => void;
  ariaLabel: string;
}) {
  if (!visible) return null;
  const badge = newTokenCount > 99 ? "99+" : newTokenCount > 0 ? String(newTokenCount) : null;

  return (
    <Button
      variant="outline"
      size="icon-lg"
      onClick={onClick}
      aria-label={ariaLabel}
      className="absolute bottom-[max(1rem,calc(1rem+env(safe-area-inset-bottom)))] right-6 z-10 rounded-full shadow-lg transition-all duration-150 ease-out"
    >
      <ChevronDown className="h-5 w-5" />
      {isStreaming && (
        <span className="absolute -top-1 -right-1 h-4 w-4 rounded-full bg-primary animate-pulse" />
      )}
      {badge && (
        <span className="absolute -bottom-1 -right-1 min-w-5 h-5 rounded-full bg-primary text-primary-foreground text-3xs font-bold flex items-center justify-center px-1 leading-none">
          {badge}
        </span>
      )}
    </Button>
  );
}

// ── Header / Footer ────────────────────────────────────────────────────────

function VirtuosoHeader({
  hiddenCount,
  onLoadEarlier,
  isLoadingHistory,
}: {
  hiddenCount: number;
  onLoadEarlier: () => void;
  isLoadingHistory: boolean;
}) {
  const { t } = useTranslation();
  if (isLoadingHistory) {
    return (
      <div className="flex justify-center py-3">
        <CircularLoader size="sm" />
      </div>
    );
  }
  if (hiddenCount <= 0) return null;
  return (
    <div className="flex items-center justify-center py-4">
      <Button
        variant="outline"
        size="sm"
        onClick={onLoadEarlier}
        className="rounded-full"
      >
        {t("chat.show_earlier", { count: hiddenCount })}
      </Button>
    </div>
  );
}

function VirtuosoFooter({ turnLimitMessage }: { turnLimitMessage: string | null }) {
  return (
    <div className="mx-auto w-full max-w-4xl px-3 md:px-6 pb-2">
      {turnLimitMessage && (
        <div
          data-testid="turn-limit-message"
          className="flex items-center gap-3 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm text-warning my-3 animate-in fade-in slide-in-from-bottom-2 duration-200"
        >
          <svg className="h-4 w-4 shrink-0" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v3.75m9-.75a9 9 0 1 1-18 0 9 9 0 0 1 18 0Zm-9 3.75h.008v.008H12v-.008Z" />
          </svg>
          <span>{turnLimitMessage}</span>
        </div>
      )}
    </div>
  );
}

// ── Main MessageList component ──────────────────────────────────────────────

export function MessageList({
  agent,
  messages,
  isStreaming,
  isTextStreaming,
  showThinking,
  isLoadingHistory,
  emptyState,
  hiddenCount,
  onLoadEarlier,
  searchMatchIds,
  searchActive,
  searchOpen,
}: {
  agent?: string;
  messages: ChatMessage[];
  isStreaming: boolean;
  /** True only during active text emission (phase === "streaming"), not during reconnect. */
  isTextStreaming: boolean;
  showThinking: boolean;
  isLoadingHistory: boolean;
  emptyState: ReactNode;
  hiddenCount: number;
  onLoadEarlier: () => void;
  /** Set of messageIds that matched the current search query. */
  searchMatchIds?: Set<string>;
  /** When true, non-matching messages are dimmed. */
  searchActive?: boolean;
  /**
   * True when the in-app SearchBar is open. The SearchBar wrapper already
   * supplies the mobile pt-14 offset above this list, so the list must NOT
   * add it again (avoids a double mobile-header offset). Desktop (lg) is
   * unaffected — lg:pt-0 is a no-op either way.
   */
  searchOpen?: boolean;
}) {
  const { t } = useTranslation();
  // Mobile-header offset. Suppressed when the SearchBar is open because its
  // wrapper already provides pt-14; desktop (lg) is always lg:pt-0.
  const topOffset = searchOpen ? "" : "pt-14 lg:pt-0";
  const storeAgent = useChatStore((s) => s.currentAgent);
  const currentAgent = agent || storeAgent;
  const activeSessionId = useChatStore((s) => s.agents[currentAgent]?.activeSessionId ?? null);
  const turnLimitMessage = useChatStore((s) => s.agents[currentAgent]?.turnLimitMessage ?? null);

  // Double-fetch guard (B3): both `startReached` (scroll) and the "show earlier"
  // button call onLoadEarlier; without a guard a fast scroll-to-top + click (or
  // Virtuoso firing startReached repeatedly) triggers duplicate page fetches.
  // The ref latches until the load settles — reset when the next page arrives
  // (hiddenCount changes, covers the synchronous render-limit path) OR when the
  // network fetch finishes (isLoadingHistory flips back to false, covering an
  // empty/failed page that leaves hiddenCount unchanged — otherwise the latch
  // would strand pagination for the rest of the session).
  const loadEarlierInFlight = useRef(false);
  useEffect(() => {
    if (!isLoadingHistory) loadEarlierInFlight.current = false;
  }, [hiddenCount, isLoadingHistory]);
  const guardedLoadEarlier = useCallback(() => {
    if (loadEarlierInFlight.current || !onLoadEarlier) return;
    loadEarlierInFlight.current = true;
    onLoadEarlier();
  }, [onLoadEarlier]);

  // ── Auto-follow logic ─────────────────────────────────────────────────────
  const {
    virtuosoRef,
    setSentinelEl,
    setScrollerEl,
    isAtTail,
    shouldFollow,
    missedTokens,
    scrollToBottom,
    trackNewTokens,
  } = useChatAutoscroll(isStreaming, activeSessionId);

  // Sync token growth with the autoscroll hook (O(1) tracking)
  useEffect(() => {
    if (messages.length > 0) {
      const last = messages[messages.length - 1];
      trackNewTokens(last.id, last.parts.length);
    }
  }, [messages, trackNewTokens]);

  // Scroll-position memory (13c): persists the first-visible message id
  // while the user is detached from the tail; cleared on return-to-bottom.
  const recordVisibleMessage = useScrollMemoryWrite(activeSessionId, shouldFollow);

  // Publish the Virtuoso handle to the module-level registry so the
  // jump-to-message hook (useScrollToMessage) can scroll imperatively without
  // prop-drilling. virtuosoRef is a stable ref object; register on mount.
  useEffect(() => {
    setVirtuosoHandle(virtuosoRef.current);
    return () => setVirtuosoHandle(null);
  }, [virtuosoRef]);

  // Hoist session data
  const { sessions: sessionRows } = useSessions(currentAgent ?? "");
  const activeSession = sessionRows.find((s) => s.id === activeSessionId);
  const sessionChannel = activeSession?.channel;
  const sessionUserId = activeSession?.user_id;

  const THINKING_ID = "__thinking__";
  const virtualItems = useMemo(() => {
    if (!showThinking) return messages;
    const thinkingItem: ChatMessage = {
      id: THINKING_ID,
      role: "assistant" as const,
      parts: [],
      createdAt: new Date().toISOString(),
    };
    return [...messages, thinkingItem];
  }, [messages, showThinking]);

  const virtuosoComponents = useMemo(() => ({
    // role=list / role=listitem on Virtuoso's own wrappers so the items are
    // DIRECT children of the list (H1) — a plain role on the itemContent div is
    // orphaned by Virtuoso's intervening scroller/list divs.
    List: VirtuosoList,
    Item: VirtuosoListItem,
    Header: () => <VirtuosoHeader hiddenCount={hiddenCount} onLoadEarlier={guardedLoadEarlier} isLoadingHistory={isLoadingHistory} />,
    Footer: () => (
      <>
        <VirtuosoFooter turnLimitMessage={turnLimitMessage} />
        <div
          ref={setSentinelEl}
          aria-hidden="true"
          data-testid="tail-sentinel"
          style={{ height: 1, width: "100%" }}
        />
      </>
    ),
  }), [hiddenCount, guardedLoadEarlier, isLoadingHistory, turnLimitMessage, setSentinelEl]);

  if (isLoadingHistory && messages.length === 0) {
    return (
      <div className={cn("flex flex-1 flex-col overflow-y-auto", topOffset)}>
        <MessageListSkeleton />
      </div>
    );
  }

  if (messages.length === 0 && !showThinking) {
    return (
      <div className={cn("flex flex-1 flex-col overflow-y-auto", topOffset)}>
        {emptyState}
      </div>
    );
  }

  return (
    <div className={cn("flex flex-1 flex-col relative overflow-hidden overscroll-contain", topOffset)}>
      <section
        role="log"
        aria-live="off"
        aria-label={t("chat.message_thread")}
        className="flex flex-1 flex-col min-h-0"
      >
      <Virtuoso
        ref={virtuosoRef}
        scrollerRef={(el) => setScrollerEl(el as HTMLElement)}
        data={virtualItems}
        computeItemKey={(_, item) => item.id}
        defaultItemHeight={120}
        alignToBottom
        skipAnimationFrameInResizeObserver
        followOutput={() => (shouldFollow ? "auto" : false)}
        atBottomThreshold={100}
        initialTopMostItemIndex={messages.length > 0 ? messages.length - 1 : 0}
        increaseViewportBy={{ top: 500, bottom: 200 }}
        rangeChanged={(range) => {
          // NOTE: rangeChanged reports the RENDERED range, which includes the
          // 500px top overscan from increaseViewportBy — react-virtuoso offers
          // no visible-only range callback, and computing one would need
          // custom scroll math. Acceptable imprecision: the restored position
          // may land up to ~500px above the exact first visible row.
          const item = virtualItems[range.startIndex];
          // Skip synthetic rows (the thinking placeholder and compression
          // dividers, id `compression-divider-{n}`) so scroll memory never
          // stores an id that has no real message to jump back to.
          const realId =
            item && item.id !== THINKING_ID && !item.id.startsWith("compression-divider-")
              ? item.id
              : null;
          recordVisibleMessage(realId);
        }}
        startReached={guardedLoadEarlier}
        components={virtuosoComponents}
        itemContent={(index, msg) => {
          if (msg.id === THINKING_ID) {
            return (
              // aria-live="off": the thread-level role="log" must NOT also
              // announce this transient row — ThinkingMessage's own role="status"
              // is the single announcer, avoiding a double "thinking" utterance.
              <div aria-live="off" className="mx-auto w-full max-w-4xl px-3 md:px-6 py-2">
                <ThinkingMessage />
              </div>
            );
          }

          const prev = index > 0 ? virtualItems[index - 1] : null;
          const showSeparator =
            prev !== null &&
            prev.id !== THINKING_ID &&
            prev.role === "assistant" &&
            msg.role === "assistant" &&
            !!prev.agentId && !!msg.agentId &&
            prev.agentId !== msg.agentId;

          // True when this assistant message continues the previous one
          // (same agent, no user message between, no agent transition).
          // After Phase 1's per-iteration UUID split, one tool-loop turn
          // produces multiple consecutive assistant ChatMessages — render
          // them as a single visual bubble instead of stacking headers.
          const continuesPrevious =
            prev !== null &&
            prev.id !== THINKING_ID &&
            prev.role === "assistant" &&
            msg.role === "assistant" &&
            !showSeparator &&
            (!prev.agentId || !msg.agentId || prev.agentId === msg.agentId);

          const isNew = (!isStreaming && isNewMessage(msg)) || (showSeparator && isStreaming);

          const isDimmed = searchActive && searchMatchIds && !searchMatchIds.has(msg.id);

          return (
            <div
              id={`msg-${msg.id}`}
              className={cn(
                "mx-auto w-full max-w-4xl px-3 md:px-6 transition-opacity duration-150",
                isDimmed && "opacity-40",
              )}
            >
              {showSeparator && <AgentTransitionDivider agentName={msg.agentId!} />}
              <div className={cn(
                isNew && "animate-in fade-in slide-in-from-bottom-2 duration-300 ease-out",
              )}>
                <MessageItem
                  message={msg}
                  sessionChannel={sessionChannel}
                  sessionUserId={sessionUserId}
                  continuesPrevious={continuesPrevious}
                />
              </div>
            </div>
          );
        }}
      />
      </section>

      <ScrollToBottomButton
        visible={!isAtTail}
        isStreaming={isStreaming}
        newTokenCount={missedTokens}
        onClick={scrollToBottom}
        ariaLabel={t("chat.scroll_to_bottom")}
      />
    </div>
  );
}
