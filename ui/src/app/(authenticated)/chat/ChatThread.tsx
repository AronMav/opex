"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { useChatStore, isActivePhase } from "@/stores/chat-store";
import { useVisualViewport } from "@/hooks/use-visual-viewport";
import { useSessionMessages } from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";

import type { SessionRow } from "@/types/api";

// Stable empty fallback — prevents new array reference on every render (avoids
// infinite useEffect loop when activeSessionIds is absent during WS reconnect).
const EMPTY_ACTIVE_IDS: string[] = [];

import { MessageList, MessageSkeleton } from "./MessageList";
import { StreamingAnnouncer } from "./StreamingAnnouncer";
import { SearchBar } from "./SearchBar";
import { ThreadErrorBoundary } from "./ThreadErrorBoundary";
import { ReconnectingIndicator } from "@/components/chat/ReconnectingIndicator";
import { ChatWelcomeScreen as EmptyState } from "./ChatWelcomeScreen";
import { ReadOnlyFooter } from "./read-only/ReadOnlyFooter";
import { ErrorBanner } from "@/components/ui/error-banner";
import { ChatComposer } from "./composer/ChatComposer";
import { ShortcutHelp } from "@/components/chat/ShortcutHelp";
import { VideoProgressIndicator } from "@/components/chat/VideoProgressIndicator";
import { useEngineRunning } from "./hooks/use-engine-running";
import { useRenderMessages } from "./hooks/use-render-messages";
import { selectIsLive, selectIsReplayingHistory, selectLiveHasContent } from "@/stores/chat-selectors";
import { useMessageSearch } from "./hooks/use-message-search";

// ── Props ────────────────────────────────────────────────────────────────────

interface ChatThreadProps {
  /** Explicit agent name this thread is for. Prevents sync issues during agent switching. */
  agent?: string;
  streamError: string | null;
  isReadOnly: boolean;
  activeSession?: SessionRow;
  onClearError: () => void;
  onRetry: () => void;
}

// ── Main Thread ──────────────────────────────────────────────────────────────

export function ChatThread({
  agent,
  streamError,
  isReadOnly,
  activeSession,
  onClearError,
  onRetry,
}: ChatThreadProps) {
  const { t } = useTranslation();
  const keyboardHeight = useVisualViewport();
  const storeAgent = useChatStore((s) => s.currentAgent);
  const currentAgent = agent || storeAgent;
  const activeSessionId = useChatStore((s) => s.agents[currentAgent]?.activeSessionId ?? null);
  const connectionPhase = useChatStore((s) => s.agents[currentAgent]?.connectionPhase ?? "idle");
  const reconnectAttempt = useChatStore((s) => s.agents[currentAgent]?.reconnectAttempt ?? 0);
  const maxReconnectAttempts = useChatStore((s) => s.agents[currentAgent]?.maxReconnectAttempts ?? 6);
  const isLlmReconnecting = useChatStore((s) => s.agents[currentAgent]?.isLlmReconnecting ?? false);
  const activeSessionIds = useChatStore((s) => s.agents[currentAgent]?.activeSessionIds ?? EMPTY_ACTIVE_IDS);

  // "Running" = active connection phase OR WS push reports the session active.
  // DB run_status is no longer consulted in the hot path (spec I3).
  const engineRunning = useEngineRunning(currentAgent);

  // ── Keyboard shortcut help (Ctrl+/) ─────────────────────────────────────────
  const [shortcutHelpOpen, setShortcutHelpOpen] = useState(false);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.key === "/") {
        e.preventDefault();
        setShortcutHelpOpen((prev) => !prev);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  // Derived booleans from message source hooks
  const isLive = useChatStore((s) => selectIsLive(s, currentAgent));
  const isHistory = useChatStore((s) => selectIsReplayingHistory(s, currentAgent));
  const liveHasContent = useChatStore((s) => selectLiveHasContent(s, currentAgent));

  // Bootstrap: on session change, if WS already marked this session active
  // (e.g. WS snapshot arrived before localStorage restored activeSessionId),
  // kick off auto-resume. Not reactive on activeSessionIds — deliberately
  // one-shot per session change. resumeStream is idempotent.
  useEffect(() => {
    if (!activeSessionId || isActivePhase(connectionPhase)) return;
    if (activeSessionIds.includes(activeSessionId)) {
      useChatStore.getState().resumeStream(currentAgent, activeSessionId);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSessionId, currentAgent]);  // intentionally NOT activeSessionIds

  // Always fetch session messages — even during streaming.
  // During live streaming, sourceMessages prefers live data, but history data
  // is needed as fallback (e.g. F5 reload while agent is processing).
  const { data: sessionMessagesData, isLoading: historyLoading } = useSessionMessages(
    activeSessionId,
    currentAgent,
  );
  // sessionMessagesData used only for showSkeleton — useRenderMessages reads via the cache

  const renderLimit = useChatStore((s) => s.agents[s.currentAgent]?.renderLimit ?? 100);

  const loadEarlierMessages = useChatStore((s) => s.loadEarlierMessages);
  const loadPreviousMessages = useChatStore((s) => s.loadPreviousMessages);
  const hasMoreHistory = useChatStore((s) => s.agents[s.currentAgent]?.hasMoreHistory ?? false);
  const isScrollLoadingHistory = useChatStore((s) => s.agents[s.currentAgent]?.isLoadingHistory ?? false);

  // Architecture C: history + SSE overlay. See `chat-overlay-dedup.ts`
  // for the status-independent user-bubble merge (fixes the 2026-04-17
  // "sent message disappears" regression).
  const sourceMessages = useRenderMessages(currentAgent);

  // Filter out inter-agent routing messages (internal inter-agent context passed between agents).
  // These have role="user" with content starting with "[Handoff from" or "[Response from".
  // Keep the original user message (no agentId or agentId matching current agent).
  const filteredMessages = useMemo(() => sourceMessages.filter(m => {
    // Skip empty assistant messages (pre-content SSE placeholders) — ThinkingMessage handles this
    if (m.role === "assistant" && m.parts.length === 0) return false;
    if (m.role !== "user" || !m.agentId) return true;
    // Keep if it's from the session's primary agent (real user proxy)
    const content = m.parts[0]?.type === "text" ? (m.parts[0] as { text: string }).text : "";
    return !content.startsWith("[Handoff from") && !content.startsWith("[Response from");
  }), [sourceMessages]);

  const allMessages = useMemo(
    () => filteredMessages.length > renderLimit ? filteredMessages.slice(-renderLimit) : filteredMessages,
    [filteredMessages, renderLimit],
  );

  const msgCount = sourceMessages.length;
  // hiddenCount is based on filteredMessages (not raw sourceMessages) so inter-agent
  // routing messages don't inflate the "load earlier" indicator.
  const hiddenCount = useMemo(() => Math.max(0, filteredMessages.length - renderLimit), [filteredMessages.length, renderLimit]);
  const hasMessages = msgCount > 0;

  const isStreaming = isActivePhase(connectionPhase);
  // Only true during active text emission — excludes "reconnecting" so the
  // streaming cursor doesn't linger after session completion while SSE reconnects.
  const isTextStreaming = connectionPhase === "streaming";

  // ── Pending message queue drain ────────────────────────────────────────────
  // When connectionPhase transitions to 'idle' (clean success), drain the
  // single-slot pending queue set by queueMessage (Shift+Enter while streaming).
  // On 'error', discard the pending message.
  const pendingMessage = useChatStore((s) => s.agents[s.currentAgent]?.pendingMessage ?? null);
  const prevPhaseRef = useRef<string>(connectionPhase);
  useEffect(() => {
    const prevPhase = prevPhaseRef.current;
    prevPhaseRef.current = connectionPhase;

    if (connectionPhase === "idle" && prevPhase !== "idle" && pendingMessage) {
      // Clean transition to idle — drain queue.
      useChatStore.getState().sendMessage(pendingMessage.content, pendingMessage.attachments);
      useChatStore.getState().clearPending(currentAgent);
    } else if (connectionPhase === "error" && pendingMessage) {
      // Stream ended in error — discard queue so user sees the error first.
      useChatStore.getState().clearPending(currentAgent);
    }
  }, [connectionPhase, pendingMessage, currentAgent]);

  const lastMsg = sourceMessages[sourceMessages.length - 1];
  // Show thinking when assistant hasn't produced text yet — covers "waiting for
  // first response" and "tool-call loop still running" (parts exist but no text).
  const lastAssistantHasText = lastMsg?.role === "assistant" && lastMsg.parts.some(
    (p) => p.type === "text" && (p as { type: string; text?: string }).text,
  );
  const lastMsgIsOtherAgent = lastMsg?.role === "assistant" && lastMsg.agentId && lastMsg.agentId !== currentAgent;
  const isLiveOrHistory = isLive || isHistory;
  // When resumeStream starts, live overlay is empty ([]) so history bleeds through —
  // the last rendered message is the previous ALMA response (has text), which
  // incorrectly suppresses showThinking. Bypass lastAssistantHasText when live
  // mode has no overlay content yet (the stream hasn't sent any events yet).
  const isLiveEmpty = isLive && !liveHasContent;
  // "complete" is the post-stream finishing window: stream ended cleanly, the
  // post-finally is awaiting RQ refetch. RQ cache is briefly stale (the
  // session status still reads "running") for ~100-500ms — without this
  // guard, the thinking animation lingers visibly after the response is
  // fully rendered.
  const showThinking = isLiveOrHistory
    && (isLiveEmpty || !lastAssistantHasText)
    && !lastMsgIsOtherAgent
    && connectionPhase !== "complete"
    && (connectionPhase === "submitted" || connectionPhase === "streaming" || connectionPhase === "reconnecting"
        || engineRunning);

  // ── Message search (Ctrl+Shift+F) ────────────────────────────────────────
  const search = useMessageSearch(allMessages);

  // Global Ctrl+Shift+F shortcut — opens in-app search and prevents browser find.
  useEffect(() => {
    const handleGlobalKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        if (search.isOpen) {
          search.close();
        } else {
          search.open();
        }
      }
    };
    window.addEventListener("keydown", handleGlobalKey);
    return () => window.removeEventListener("keydown", handleGlobalKey);
  }, [search]);

  // Pre-compute matched message IDs for dimming (stable Set reference).
  const searchMatchIds = useMemo(() => {
    if (!search.isOpen || !search.query) return null;
    return new Set(search.matches.map((m) => m.messageId));
  }, [search.isOpen, search.query, search.matches]);

  // Only show loading skeleton when there is truly no data to display (Fix D).
  // If we have cached history, skip the skeleton.
  // Regression 2026-04-17: previously `messageSource.mode !== "live"` skipped
  // the skeleton for live mode even when the live overlay was empty — on F5
  // during an active stream, `resumeStream` sets live:[] and history is still
  // loading, leaving the user with a BLANK chat until SSE events arrive. Now
  // we also show the skeleton when live overlay is empty AND history is still
  // loading, so the user sees a proper loading indicator instead of emptiness.
  const liveIsEmpty = isLive && !liveHasContent;
  const showSkeleton =
    historyLoading && !sessionMessagesData &&
    (!isLive || liveIsEmpty);
  if (showSkeleton) {
    return (
      <div className="flex flex-1 flex-col gap-6 p-6 max-w-4xl mx-auto">
        {[1, 2, 3].map((i) => (
          <MessageSkeleton key={i} />
        ))}
      </div>
    );
  }

  return (
    <ThreadErrorBoundary retryLabel={t("chat.retry")} onRetry={onRetry}>
    <div
      className="flex flex-1 flex-col min-h-0 relative"
      style={keyboardHeight > 0 ? { paddingBottom: keyboardHeight } : undefined}
    >
      {search.isOpen && (
        <div>
          <SearchBar search={search} />
        </div>
      )}
      <StreamingAnnouncer agent={currentAgent} />
      <MessageList
        agent={currentAgent}
        messages={allMessages}
        isStreaming={isStreaming}
        isTextStreaming={isTextStreaming}
        showThinking={showThinking}
        searchOpen={search.isOpen}

        isLoadingHistory={(historyLoading && !liveHasContent) || isScrollLoadingHistory}
        emptyState={<EmptyState />}
        hiddenCount={hiddenCount}
        onLoadEarlier={
          hasMoreHistory
            ? () => loadPreviousMessages(currentAgent)
            : () => loadEarlierMessages(currentAgent)
        }
        searchMatchIds={searchMatchIds ?? undefined}
        searchActive={search.isOpen && !!search.query}
      />

      {/* Reconnecting indicator — SSE transport reconnect OR LLM-level retry */}
      {(connectionPhase === "reconnecting" || isLlmReconnecting) && (
        <ReconnectingIndicator
          attempt={reconnectAttempt}
          maxAttempts={maxReconnectAttempts}
          className="my-4"
        />
      )}

      {/* Error banner — also shown when the engine was interrupted mid-stream
          and the resume endpoint returns a sync event with status="interrupted"
          (Fix #5). After such an event the stream falls into history mode but
          the banner must still be visible so the user knows the previous run
          died and didn't just finish silently. */}
      {streamError && !isReadOnly && (
        <ErrorBanner
          error={streamError}
          hasMessages={hasMessages}
          onClear={onClearError}
          onRetry={onRetry}
        />
      )}

      {/* Video processing progress indicator */}
      <VideoProgressIndicator sessionId={activeSessionId} />

      {/* Input area */}
      {isReadOnly ? (
        <ReadOnlyFooter activeSession={activeSession} />
      ) : (
        <ChatComposer />
      )}

      {/* Keyboard shortcut help overlay */}
      <ShortcutHelp open={shortcutHelpOpen} onOpenChange={setShortcutHelpOpen} />
    </div>
    </ThreadErrorBoundary>
  );
}
