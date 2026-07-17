"use client";

import React, { useMemo } from "react";
import { useChatStore } from "@/stores/chat-store";
import { useAutoPaginateWhileFiltering } from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import { relativeTime } from "@/lib/format";
import { Loader } from "@/components/ui/loader";
import { Virtuoso } from "react-virtuoso";
import { VirtuosoList, VirtuosoListItem } from "@/components/chat/virtuoso-list-roles";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Input } from "@/components/ui/input";
import { Plus, Search, Trash2, Pencil, Share2 } from "lucide-react";
import { ParentBadge } from "@/components/chat/ParentBadge";
import { TaskPlanPanel } from "@/components/TaskPlanPanel";
import type { SessionRow } from "@/types/api";

export interface SessionSidebarProps {
  currentAgent: string;
  isStreaming: boolean;
  sessions: SessionRow[];
  sessionsLoading: boolean;
  sessionsTotal: number;
  fetchNextPage: () => void;
  hasNextPage: boolean;
  isFetchingNextPage: boolean;
  activeSessionId: string | null;
  activeSessionIds: string[];
  selectedSessions: Set<string>;
  deletingSelected: boolean;
  deletingSessionId: string | null;
  sharingSessionId: string | null;
  sessionFilter: string;
  setSessionFilter: React.Dispatch<React.SetStateAction<string>>;
  renamingSessionId: string | null;
  setRenamingSessionId: React.Dispatch<React.SetStateAction<string | null>>;
  renameValue: string;
  setRenameValue: React.Dispatch<React.SetStateAction<string>>;
  onNewChat: () => void;
  onSelectSession: (session: { id: string; agent_id: string }) => void;
  onDeleteSessions: () => void;
  onDeleteSession: (e: React.MouseEvent, sessionId: string) => void;
  onShareSession: (e: React.MouseEvent, sessionId: string) => void;
  toggleSessionSelection: (sessionId: string) => void;
}

/**
 * Session sidebar (list + filter + multi-select + per-session actions),
 * extracted verbatim from chat/page.tsx. Presentational: all sidebar state and
 * handlers stay in page.tsx and are passed as props so the desktop pane and the
 * mobile Sheet keep sharing one state instance (zero behavioural change).
 */
export function SessionSidebar({
  currentAgent,
  isStreaming,
  sessions,
  sessionsLoading,
  sessionsTotal,
  fetchNextPage,
  hasNextPage,
  isFetchingNextPage,
  activeSessionId,
  activeSessionIds,
  selectedSessions,
  deletingSelected,
  deletingSessionId,
  sharingSessionId,
  sessionFilter,
  setSessionFilter,
  renamingSessionId,
  setRenamingSessionId,
  renameValue,
  setRenameValue,
  onNewChat,
  onSelectSession,
  onDeleteSessions,
  onDeleteSession,
  onShareSession,
  toggleSessionSelection,
}: SessionSidebarProps) {
  const { t, locale } = useTranslation();

  // Filtered sessions
  const filteredSessions = useMemo(() =>
    sessionFilter
      ? sessions.filter((s) => {
          const q = sessionFilter.toLowerCase();
          return (
            (s.title && s.title.toLowerCase().includes(q)) ||
            (s.user_id && s.user_id.toLowerCase().includes(q)) ||
            s.channel.toLowerCase().includes(q) ||
            s.id.toLowerCase().includes(q)
          );
        })
      : sessions,
    [sessions, sessionFilter],
  );

  // While a filter is active, Virtuoso's endReached never fires on a short
  // filtered list — so proactively pull older pages until the filter has enough
  // to match against (or the server runs out).
  useAutoPaginateWhileFiltering({
    filterActive: !!sessionFilter,
    visibleCount: filteredSessions.length,
    hasNextPage,
    isFetchingNextPage,
    fetchNextPage,
  });

  return (
    <div className="flex h-full flex-col bg-sidebar">
      <TaskPlanPanel agentName={currentAgent} isStreaming={isStreaming} />
      <div className="flex items-center justify-between px-3 py-3 md:px-5 md:py-5 border-b border-border/50">
        <div className="flex flex-col gap-1">
          <span className="text-sm font-display font-semibold text-foreground">
            {t("chat.sessions")}
          </span>
          <span className="text-xs text-muted-foreground-subtle">
            {sessionsTotal > sessions.length
              ? t("chat.sessions_count_of", { loaded: sessions.length, total: sessionsTotal })
              : t("chat.sessions_count", { count: sessionsTotal })}
          </span>
        </div>
        <div className="flex items-center gap-1.5">
          {sessions.length > 0 && (
            <Button
              variant="ghost"
              size="sm"
              className={`h-8 px-2 text-xs transition-colors ${
                selectedSessions.size > 0
                  ? "text-destructive bg-destructive/10 hover:bg-destructive/30"
                  : "text-muted-foreground hover:text-destructive hover:bg-destructive/10"
              }`}
              onClick={onDeleteSessions}
              disabled={deletingSelected}
              title={selectedSessions.size > 0
                ? t("chat.delete_selected")
                : t("chat.delete_all_sessions", { agent: currentAgent })}
            >
              {deletingSelected ? (
                <Loader className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Trash2 className="h-3.5 w-3.5" />
              )}
              {selectedSessions.size > 0 && (
                <span className="ml-1">{selectedSessions.size}</span>
              )}
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            className="hidden lg:inline-flex h-8 px-3 border-primary/30 !bg-primary/10 text-primary text-xs font-medium transition-all hover:bg-primary/10 hover:text-primary hover:border-primary/30"
            onClick={onNewChat}
          >
            <Plus className="mr-1.5 h-4 w-4" /> {t("chat.new")}
          </Button>
        </div>
      </div>

      <div className="shrink-0 px-3 py-2 border-b border-border/30">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground-subtle" />
          <Input
            value={sessionFilter}
            onChange={(e) => setSessionFilter(e.target.value)}
            placeholder={t("chat.search_sessions")}
            className="h-8 pl-8 text-xs bg-muted/30 border-border/50 placeholder:text-muted-foreground-subtle"
          />
        </div>
      </div>
      <div className="flex-1 min-h-0 px-3 relative overflow-hidden">
        {sessionsLoading && sessions.length === 0 ? (
          <div className="space-y-4 p-3">
            {[1, 2, 3].map((i) => (
              <div key={i} className="space-y-2">
                <Skeleton className="h-3 w-16 bg-muted/30" />
                <Skeleton className="h-4 w-full bg-muted/30" />
              </div>
            ))}
          </div>
        ) : filteredSessions.length === 0 ? (
          <div className="flex h-32 items-center justify-center rounded-lg border border-dashed border-border px-6 text-center">
            <p className="text-sm text-muted-foreground-subtle">
              {sessionFilter ? t("chat.no_sessions_match") : t("chat.no_sessions")}
            </p>
          </div>
        ) : (
          <div className="h-full">
            <Virtuoso
              data={filteredSessions}
              className="!h-full scrollbar-none"
              endReached={() => {
                if (hasNextPage && !isFetchingNextPage) fetchNextPage();
              }}
              components={{
                List: VirtuosoList,
                Item: VirtuosoListItem,
                Footer: () =>
                  isFetchingNextPage ? (
                    <div className="flex justify-center py-3">
                      <Loader className="h-4 w-4 animate-spin text-muted-foreground-subtle" />
                    </div>
                  ) : null,
              }}
              itemContent={(_index, s) => {
                const isSelected = selectedSessions.has(s.id);
                const displayTitle = s.title || s.user_id || t("chat.no_title");
                return (
                  <div className="group relative pb-1.5 flex items-stretch gap-1 min-w-0">
                    <button
                      onClick={() => toggleSessionSelection(s.id)}
                      className={`shrink-0 self-center h-5 w-5 md:h-3.5 md:w-3.5 rounded border transition-colors flex items-center justify-center cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background ${
                        isSelected
                          ? "bg-primary border-primary"
                          : "border-border/50 bg-transparent hover:border-primary/30"
                      }`}
                      role="checkbox"
                      aria-checked={isSelected}
                      aria-label={t("chat.select_session")}
                    >
                      {isSelected && (
                        <svg className="h-3.5 w-3.5 md:h-2.5 md:w-2.5 text-primary-foreground" viewBox="0 0 10 10" fill="none">
                          <path d="M2 5l2.5 2.5L8 3" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
                        </svg>
                      )}
                    </button>
                    <button
                      onClick={() => onSelectSession(s)}
                      className={`relative flex w-full min-w-0 flex-col gap-1.5 rounded-lg px-3 py-2.5 pb-9 md:px-4 md:py-3 md:pb-3 md:pr-14 text-left transition-all duration-300 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background overflow-hidden ${
                        activeSessionId === s.id
                        ? "bg-accent shadow-inner"
                        : "hover:bg-accent/40"
                      }`}
                    >
                      <div className="flex items-center justify-between gap-2 flex-wrap">
                        <div className="flex items-center gap-1 min-w-0 flex-1">
                          <span
                            className={`font-display text-xs font-bold uppercase tracking-tight shrink-0 ${
                              activeSessionId === s.id
                                ? "text-primary"
                                : "text-muted-foreground-subtle group-hover:text-muted-foreground"
                            }`}
                          >
                            {s.channel}
                          </span>
                          {(activeSessionIds.includes(s.id) || s.run_status === "running") ? (
                            <span className="ml-1.5 rounded px-1.5 py-0.5 font-mono text-3xs uppercase tracking-wider bg-success/15 text-success flex items-center gap-1 shrink-0">
                              <span className="h-3 w-3 rounded-full bg-success animate-pulse" />
                              {t("chat.status_running")}
                            </span>
                          ) : (s.run_status === "interrupted" || s.run_status === "timeout" || s.run_status === "failed") ? (
                            <span className="ml-1 rounded px-1 py-0.5 font-mono text-3xs uppercase tracking-wider bg-destructive/10 text-destructive/80 shrink-0">
                              {s.run_status === "interrupted" ? t("chat.status_interrupted") : s.run_status === "timeout" ? t("chat.status_timeout") : t("chat.status_failed")}
                            </span>
                          ) : null}
                        </div>
                        {/* Participant avatars removed — agents are now session-scoped via agent tool */}
                        <span className="font-mono text-xs tabular-nums text-muted-foreground-subtle shrink-0">
                          {relativeTime(s.last_message_at, locale)}
                        </span>
                      </div>
                      {renamingSessionId === s.id ? (
                        <input
                          autoFocus
                          value={renameValue}
                          onChange={(e) => setRenameValue(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") {
                              e.preventDefault();
                              useChatStore.getState().renameSession(s.id, renameValue);
                              setRenamingSessionId(null);
                            } else if (e.key === "Escape") {
                              setRenamingSessionId(null);
                            }
                          }}
                          onBlur={() => {
                            if (renameValue !== (s.title || "")) {
                              useChatStore.getState().renameSession(s.id, renameValue);
                            }
                            setRenamingSessionId(null);
                          }}
                          className="w-full truncate text-sm bg-transparent border-b border-primary outline-none focus-visible:ring-1 focus-visible:ring-ring text-foreground placeholder:text-muted-foreground-subtle"
                          placeholder={t("chat.rename_session")}
                        />
                      ) : (
                        <>
                          <p
                            className={`text-sm transition-colors break-words line-clamp-2 ${
                              activeSessionId === s.id
                                ? "text-foreground"
                                : "text-muted-foreground group-hover:text-muted-foreground"
                            } ${!s.title && !s.user_id ? "italic text-muted-foreground-subtle" : ""}`}
                          >
                            {displayTitle}
                            {s.segment_count != null && s.segment_count > 1 && (
                              <span className="ml-1.5 text-xs text-muted-foreground-subtle tabular-nums not-italic whitespace-nowrap">
                                ◈{s.segment_count}
                              </span>
                            )}
                          </p>
                          {s.parent_session_id && (
                            <ParentBadge
                              parentTitle={
                                sessions.find((p) => p.id === s.parent_session_id)?.title ?? null
                              }
                              onNavigate={() =>
                                useChatStore.getState().selectSession(s.parent_session_id!, currentAgent)
                              }
                            />
                          )}
                        </>
                      )}
                      {activeSessionId === s.id && (
                        <div className="absolute left-0 top-1/2 -translate-y-1/2 h-8 w-0.5 rounded-full bg-primary" />
                      )}
                    </button>
                    <div className="absolute right-1.5 bottom-1 flex flex-row md:right-2 md:top-2 md:bottom-auto md:flex-col items-center gap-0.5 opacity-100 md:opacity-0 md:group-hover:opacity-100 md:group-focus-within:opacity-100 transition-opacity duration-150 z-10">
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          onClick={(e) => {
                            e.stopPropagation();
                            setRenamingSessionId(s.id);
                            setRenameValue(s.title || "");
                          }}
                          className="text-muted-foreground-subtle hover:text-foreground"
                          title={t("chat.rename_hint")}
                        >
                          <Pencil className="h-4 w-4" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          onClick={(e) => onShareSession(e, s.id)}
                          disabled={sharingSessionId === s.id}
                          className="text-muted-foreground-subtle hover:text-foreground"
                          title={t("chat.share_session")}
                        >
                          <Share2 className="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          onClick={(e) => onDeleteSession(e, s.id)}
                          disabled={deletingSessionId === s.id}
                          className="text-muted-foreground-subtle hover:bg-destructive/10 hover:text-destructive"
                          title={t("chat.delete_session")}
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                        </Button>
                      </div>
                  </div>
                );
              }}
            />
          </div>
        )}
      </div>
    </div>
  );
}
