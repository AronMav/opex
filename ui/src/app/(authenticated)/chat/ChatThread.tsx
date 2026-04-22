"use client";

import React, { Component, useEffect, useMemo } from "react";
import type { ErrorInfo, ReactNode } from "react";
import { useChatStore, isActivePhase } from "@/stores/chat-store";
import { useVisualViewport } from "@/hooks/use-visual-viewport";
import { useSessionMessages, useSessions } from "@/lib/queries";

import type { SessionRow } from "@/types/api";
// ── Re-exports for backward compatibility ────────────────────────────────────
export { ToolCallPartView } from "@/components/chat/ToolCallPartView";
export { FileDataPartView } from "@/components/chat/FileDataPartView";

import { MessageList, MessageSkeleton } from "./MessageList";
import { ReconnectingIndicator } from "@/components/chat/ReconnectingIndicator";
import { EmptyState } from "./EmptyState";
import { ReadOnlyFooter } from "./read-only/ReadOnlyFooter";
import { ErrorBanner } from "./error/ErrorBanner";
import { ChatComposer } from "./composer/ChatComposer";
import { useEngineRunning } from "./hooks/use-engine-running";
import { useRenderMessages } from "./hooks/use-render-messages";
import { useIsLive } from "./hooks/use-is-live";
import { useIsReplayingHistory } from "./hooks/use-is-replaying-history";
import { useLiveHasContent } from "./hooks/use-live-has-content";

// ── Props ────────────────────────────────────────────────────────────────────

interface ChatThreadProps {
  streamError: string | null;
  isReadOnly: boolean;
  activeSession?: SessionRow;
  onClearError: () => void;
  onRetry: () => void;
}

// ── Thread Error Boundary ────────────────────────────────────────────────────

interface ThreadErrorBoundaryProps { children: ReactNode; onRetry?: () => void }
interface ThreadErrorBoundaryState { error: string | null }

class ThreadErrorBoundary extends Component<ThreadErrorBoundaryProps, ThreadErrorBoundaryState> {
  state: ThreadErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error: error.message };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.warn("[ThreadErrorBoundary]", error.message, info.componentStack?.slice(0, 200));
  }

  render() {
    if (this.state.error) {
      return (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
          <p className="text-sm text-muted-foreground/70 font-mono">{this.state.error}</p>
          <button
            className="px-4 py-2 text-sm rounded-lg border border-border bg-card hover:bg-muted transition-colors"
            onClick={() => this.setState({ error: null })}
          >
            Retry
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

// ── Main Thread ──────────────────────────────────────────────────────────────

export function ChatThread({
  streamError,
  isReadOnly,
  activeSession,
  onClearError,
  onRetry,
}: ChatThreadProps) {
  const keyboardHeight = useVisualViewport();
  const currentAgent = useChatStore((s) => s.currentAgent);
  const activeSessionId = useChatStore((s) => s.agents[s.currentAgent]?.activeSessionId ?? null);
  const connectionPhase = useChatStore((s) => s.agents[s.currentAgent]?.connectionPhase ?? "idle");
  const reconnectAttempt = useChatStore((s) => s.agents[s.currentAgent]?.reconnectAttempt ?? 0);
  const maxReconnectAttempts = useChatStore((s) => s.agents[s.currentAgent]?.maxReconnectAttempts ?? 3);
  const isLlmReconnecting = useChatStore((s) => s.agents[s.currentAgent]?.isLlmReconnecting ?? false);
  const activeSessionIds = useChatStore((s) => s.agents[s.currentAgent]?.activeSessionIds ?? []);
  // Engine running: either WS says it's active, OR React Query sessions list says run_status=running
  const { data: sessionsData } = useSessions(currentAgent);
  const sessionRunStatus = sessionsData?.sessions?.find((s: { id: string }) => s.id === activeSessionId)?.run_status;

  // CRITICAL: We are "running" if we're in an active connection phase OR the DB says so.
  const engineRunning = useEngineRunning(currentAgent);

  // Derived booleans from message source hooks
  const isLive = useIsLive(currentAgent);
  const isHistory = useIsReplayingHistory(currentAgent);
  const liveHasContent = useLiveHasContent(currentAgent);

  // Auto-resume SSE stream when engine is still processing. React 18+ batches
  // state updates; isActivePhase + isRunning guards prevent double-fire.
  useEffect(() => {
    if (!activeSessionId || isActivePhase(connectionPhase)) return;
    const isRunning = activeSessionIds.includes(activeSessionId) || sessionRunStatus === "running";
    if (!isRunning) return;
    useChatStore.getState().resumeStream(currentAgent, activeSessionId);
  }, [activeSessionId, activeSessionIds, sessionRunStatus, connectionPhase, currentAgent]);

  // Always fetch session messages — even during streaming.
  // During live streaming, sourceMessages prefers live data, but history data
  // is needed as fallback (e.g. F5 reload while agent is processing).
  const { data: sessionMessagesData, isLoading: historyLoading } = useSessionMessages(
    activeSessionId,
    engineRunning,
  );
  // sessionMessagesData used only for showSkeleton — useRenderMessages reads via the cache

  const renderLimit = useChatStore((s) => s.agents[s.currentAgent]?.renderLimit ?? 100);

  const loadEarlierMessages = useChatStore((s) => s.loadEarlierMessages);

  // Architecture C: history + SSE overlay. See `chat-overlay-dedup.ts`
  // for the status-independent user-bubble merge (fixes the 2026-04-17
  // "sent message disappears" regression).
  const sourceMessages = useRenderMessages(currentAgent);

  // Filter out inter-agent routing messages (internal inter-agent context passed between agents).
  // These have role="user" with content starting with "[Handoff from" or "[Response from".
  // Keep the original user message (no agentId or agentId matching current agent).
  const allMessages = useMemo(() => {
    const filtered = sourceMessages.filter(m => {
      // Skip empty assistant messages (pre-content SSE placeholders) — ThinkingMessage handles this
      if (m.role === "assistant" && m.parts.length === 0) return false;
      if (m.role !== "user" || !m.agentId) return true;
      // Keep if it's from the session's primary agent (real user proxy)
      const content = m.parts[0]?.type === "text" ? (m.parts[0] as { text: string }).text : "";
      return !content.startsWith("[Handoff from") && !content.startsWith("[Response from");
    });
    return filtered.length > renderLimit ? filtered.slice(-renderLimit) : filtered;
  }, [sourceMessages, renderLimit]);

  const msgCount = sourceMessages.length;
  const hiddenCount = useMemo(() => Math.max(0, msgCount - renderLimit), [msgCount, renderLimit]);
  const hasMessages = msgCount > 0;

  const isStreaming = isActivePhase(connectionPhase);

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
  const showThinking = isLiveOrHistory
    && (isLiveEmpty || !lastAssistantHasText)
    && !lastMsgIsOtherAgent
    && (connectionPhase === "submitted" || connectionPhase === "streaming" || connectionPhase === "reconnecting"
        || engineRunning || sessionRunStatus === "running");

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
    <ThreadErrorBoundary>
    <div
      className="flex flex-1 flex-col min-h-0 relative"
      style={keyboardHeight > 0 ? { paddingBottom: keyboardHeight } : undefined}
    >
      <MessageList
        messages={allMessages}
        isStreaming={isStreaming}
        showThinking={showThinking}
        isLoadingHistory={historyLoading && !liveHasContent}
        emptyState={<EmptyState />}
        hiddenCount={hiddenCount}
        onLoadEarlier={() => loadEarlierMessages(currentAgent)}
      />

      {/* Reconnecting indicator — SSE transport reconnect OR LLM-level retry */}
      {(connectionPhase === "reconnecting" || isLlmReconnecting) && (
        <ReconnectingIndicator
          attempt={reconnectAttempt}
          maxAttempts={maxReconnectAttempts}
          className="my-4"
        />
      )}

      {/* Error banner */}
      {streamError && !isReadOnly && !isHistory && (
        <ErrorBanner
          error={streamError}
          hasMessages={hasMessages}
          onClear={onClearError}
          onRetry={onRetry}
        />
      )}

      {/* Input area */}
      {isReadOnly ? (
        <ReadOnlyFooter activeSession={activeSession} />
      ) : (
        <ChatComposer />
      )}
    </div>
    </ThreadErrorBoundary>
  );
}
