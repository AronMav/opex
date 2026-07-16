"use client";

import React, { useEffect, useState, useCallback, useMemo } from "react";
import { useShallow } from "zustand/react/shallow";
import { useAuthStore } from "@/stores/auth-store";
import {
  useChatStore,
  isActivePhase,
} from "@/stores/chat-store";
import { useWsSubscription } from "@/hooks/use-ws-subscription";
import { useHotkey } from "@/hooks/use-hotkey";
import { ChatRuntimeProvider } from "@/providers/assistant-runtime";
import { useTranslation } from "@/hooks/use-translation";
import { relativeTime } from "@/lib/format";
import { cn } from "@/lib/utils";
import { toast } from "sonner";

import { Loader } from "@/components/ui/loader";
import { Virtuoso } from "react-virtuoso";
import { VirtuosoList, VirtuosoListItem } from "@/components/chat/virtuoso-list-roles";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Sheet, SheetContent, SheetTitle, SheetTrigger } from "@/components/ui/sheet";
import { SidebarTrigger } from "@/components/ui/sidebar";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import {
  Plus,
  Clock,
  Search,
  Trash2,
  Pencil,
  Share2,
  PanelRight,
  MessageSquare,
} from "lucide-react";
import { Input } from "@/components/ui/input";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ChatThread } from "./ChatThread";
import { ContextBar } from "./ContextBar";
import { CanvasPanel } from "./CanvasPanel";
import { ParentBadge } from "@/components/chat/ParentBadge";
import { CompactChainBanner } from "@/components/chat/CompactChainBanner";
import { useCanvasStore } from "@/stores/canvas-store";
import { useSessions, useAgents, qk } from "@/lib/queries";
import { useAgentTextModel } from "@/hooks/use-profiles";
import { queryClient } from "@/lib/query-client";
import { shareSession } from "@/lib/api";
import type { SessionRow } from "@/types/api";
import { TaskPlanPanel } from "@/components/TaskPlanPanel";
import { useSessionRestore } from "./hooks/use-session-restore";

const EMPTY_SESSIONS: SessionRow[] = [];
const EMPTY_ACTIVE: string[] = [];

export default function ChatPage() {
  const { t, locale } = useTranslation();
  const { agents, refreshIfStale } = useAuthStore(
    useShallow((s) => ({ agents: s.agents, refreshIfStale: s.refreshIfStale })),
  );

  // ── Store (granular selectors to avoid re-renders during streaming) ──
  const currentAgent = useChatStore((s) => s.currentAgent);
  const { data: sessionsData, isLoading: sessionsLoading } = useSessions(currentAgent ?? "");
  const sessions = sessionsData?.sessions ?? EMPTY_SESSIONS;
  const sessionsTotal = sessionsData?.total ?? sessions.length;
  const activeSessionId = useChatStore((s) => s.agents[s.currentAgent]?.activeSessionId ?? null);
  const activeSessionIds = useChatStore((s) => s.agents[s.currentAgent]?.activeSessionIds ?? EMPTY_ACTIVE);
  const streamError = useChatStore((s) => s.agents[s.currentAgent]?.streamError ?? null);
  const isStreaming = isActivePhase(useChatStore((s) => s.agents[s.currentAgent]?.connectionPhase ?? "idle"));
  const contextTokensLive = useChatStore((s) => s.agents[s.currentAgent]?.contextTokens ?? null);
  // For inactive sessions fall back to last_input_tokens stored in the session list.
  const activeSessionLastTokens = useMemo(() => {
    if (contextTokensLive != null) return contextTokensLive;
    return sessions.find((s) => s.id === activeSessionId)?.last_input_tokens ?? null;
  }, [contextTokensLive, sessions, activeSessionId]);
  const contextTokens = activeSessionLastTokens;
  const cacheReadTokens = useChatStore((s) => s.agents[s.currentAgent]?.cacheReadTokens ?? null);
  const cacheCreationTokens = useChatStore((s) => s.agents[s.currentAgent]?.cacheCreationTokens ?? null);
  const reasoningTokens = useChatStore((s) => s.agents[s.currentAgent]?.reasoningTokens ?? null);
  const modelContextLimit = useChatStore((s) => s.agents[s.currentAgent]?.modelContextLimit ?? null);
  const modelOverride = useChatStore((s) => s.agents[s.currentAgent]?.modelOverride ?? null);
  const { data: agentsData } = useAgents();
  const currentAgentProfile = agentsData?.find((a) => a.name === currentAgent)?.profile;
  const { defaultModel: currentAgentDefaultModel } = useAgentTextModel(currentAgentProfile);
  const currentAgentModel = useMemo(() => {
    if (modelOverride) return modelOverride;
    return currentAgentDefaultModel || null;
  }, [modelOverride, currentAgentDefaultModel]);

  // Refresh agent icons if stale (>60s since last fetch)
  useEffect(() => { refreshIfStale(); }, [refreshIfStale]);

  // Detect read-only sessions (heartbeat, cron, inter-agent)
  const activeSession = sessions.find((s) => s.id === activeSessionId);
  const isReadOnly = activeSession?.channel === "heartbeat" || activeSession?.channel === "cron" || activeSession?.channel === "inter-agent";

  // Session restore on mount or agent switch.
  // IMPORTANT: Wait until sessions are ACTUALLY loaded (not just isLoading=false with empty data).
  // React Query can report isLoading=false before the first fetch completes (initial state).
  const sessionsReady = !sessionsLoading && sessionsData !== undefined;

  // Session-restore state machine: override state, cross-agent deep-link resolver,
  // 5-priority restore, and activeSessionId → URL ?s= sync. Extracted verbatim.
  const { setOverrideUrlSession, restoredAgents } = useSessionRestore({
    currentAgent,
    sessions,
    sessionsReady,
    activeSessionId,
    agents,
  });

  // Refresh session list and currently viewed session when backend finishes processing
  useWsSubscription("session_updated", useCallback(() => {
    const s = useChatStore.getState();
    const agentState = s.agents[s.currentAgent];
    
    // Always refresh the session list to show latest snippet/timestamp
    queryClient.invalidateQueries({ queryKey: qk.sessions(s.currentAgent) });
    
    // If we're looking at the updated session, sync our local state with DB
    if (agentState?.activeSessionId) {
      // Invalidate message cache so useSessionMessages() picks up the changes
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(agentState.activeSessionId) });
      
      // If NOT actively streaming, force a refresh of the history to ensure consistency
      // between live SSE-built state and final DB state.
      if (!isActivePhase(agentState.connectionPhase)) {
        s.refreshHistory(agentState.activeSessionId);
      }
    }
  }, []));

  // Server-driven session status via WS agent_processing events.
  // Backend sends initial state on WS connect, then start/end events in real-time.
  // This updates activeSessionIds in Zustand — the single source of truth for "is session running?".
  useWsSubscription("agent_processing", useCallback((data) => {
    if (!data.session_id) return;
    const store = useChatStore.getState();
    if (data.status === "start") {
      store.markSessionActive(data.agent, data.session_id);
    } else {
      store.markSessionInactive(data.agent, data.session_id);
      // Refetch sessions to get final title, message count, run_status
      queryClient.invalidateQueries({ queryKey: qk.sessions(data.agent) });
    }
  }, []));

  useWsSubscription("file_job_progress", useCallback((data: {
    job_id: string; handler_id: string; session_id: string; phase: string; pct: number; status: string;
  }) => {
    const store = useChatStore.getState();
    if (data.status === "done" || data.status === "failed") {
      store.clearVideoProgress(data.session_id);
      queryClient.invalidateQueries({ queryKey: qk.sessionMessages(data.session_id) });
    } else {
      store.setVideoProgress(data.session_id, data.phase, data.phase);
    }
  }, []));

  // Another agent was invited into this session (multi-agent sessions) — keep
  // the participant list in the chat store in sync so the UI can render it.
  useWsSubscription("agent_joined", useCallback((data) => {
    useChatStore.getState().updateSessionParticipants(data.session_id, data.participants);
  }, []));

  // approval_requested handler moved to layout.tsx (must be visible on any page)

  const [sheetOpen, setSheetOpen] = useState(false);
  const [deletingSessionId, setDeletingSessionId] = useState<string | null>(null);
  const [sharingSessionId, setSharingSessionId] = useState<string | null>(null);
  const [sessionFilter, setSessionFilter] = useState("");
  const [renamingSessionId, setRenamingSessionId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");

  // ── Multi-select & delete state ──
  const [selectedSessions, setSelectedSessions] = useState<Set<string>>(new Set());
  const [deletingSelected, setDeletingSelected] = useState(false);
  const [deleteAllOpen, setDeleteAllOpen] = useState(false);
  const [deletingAll, setDeletingAll] = useState(false);

  // Clear selection when agent changes
  useEffect(() => {
    setSelectedSessions(new Set());
  }, [currentAgent]);

  const toggleSessionSelection = useCallback((sessionId: string) => {
    setSelectedSessions((prev) => {
      const next = new Set(prev);
      if (next.has(sessionId)) {
        next.delete(sessionId);
      } else {
        next.add(sessionId);
      }
      return next;
    });
  }, []);

  const handleDeleteSessions = useCallback(async () => {
    if (selectedSessions.size === 0) {
      setDeleteAllOpen(true);
      return;
    }
    setDeletingSelected(true);
    try {
      // Cancel any in-flight refetches so intermediate responses don't cache
      // a stale  count while deletes are in progress.
      await queryClient.cancelQueries({ queryKey: qk.sessions(currentAgent) });
      await Promise.all(
        Array.from(selectedSessions).map((id) =>
          // skipInvalidation=true: suppress per-call cache invalidation so
          // we issue exactly one invalidation after all deletes complete.
          useChatStore.getState().deleteSession(id, true),
        ),
      );
      queryClient.invalidateQueries({ queryKey: qk.sessions(currentAgent) });
      toast.success(t("chat.sessions_deleted"));
      setSelectedSessions(new Set());
    } catch {
      toast.error(t("chat.sessions_delete_error"));
    } finally {
      setDeletingSelected(false);
    }
  }, [selectedSessions, currentAgent, t]);

  const handleDeleteAll = useCallback(async () => {
    setDeletingAll(true);
    try {
      await useChatStore.getState().deleteAllSessions();
      queryClient.invalidateQueries({ queryKey: qk.sessions(currentAgent) });
      toast.success(t("chat.sessions_deleted"));
      setSelectedSessions(new Set());
    } catch {
      toast.error(t("chat.sessions_delete_error"));
    } finally {
      setDeletingAll(false);
      setDeleteAllOpen(false);
    }
  }, [currentAgent, t]);

  const handleDeleteSession = useCallback(async (e: React.MouseEvent, sessionId: string) => {
    e.stopPropagation();
    setDeletingSessionId(sessionId);
    try {
      await useChatStore.getState().deleteSession(sessionId);
      toast.success(t("chat.session_deleted"));
    } catch {
      toast.error(t("chat.session_delete_error"));
    } finally {
      setDeletingSessionId(null);
    }
  }, [t]);

  const handleShareSession = useCallback(async (e: React.MouseEvent, sessionId: string) => {
    e.stopPropagation();
    setSharingSessionId(sessionId);
    try {
      const res = await shareSession(sessionId, currentAgent);
      if (!res.ok || !res.token) {
        toast.error(res.error ?? t("chat.share_error"));
        return;
      }
      const url = `${window.location.origin}/share?token=${res.token}`;
      try {
        await navigator.clipboard.writeText(url);
        toast.success(t("chat.share_copied"));
      } catch {
        // Clipboard blocked (non-HTTPS / permissions) — still surface the link.
        toast.success(t("chat.share_created"), { description: url });
      }
    } catch {
      toast.error(t("chat.share_error"));
    } finally {
      setSharingSessionId(null);
    }
  }, [currentAgent, t]);

  const handleNewChat = useCallback(() => {
    useChatStore.getState().newChat();
    // Focus composer input after new chat
    setTimeout(() => {
      const input = document.querySelector<HTMLTextAreaElement>('[role="textbox"], textarea[placeholder]');
      input?.focus();
    }, 100);
  }, []);

  const handleRegenerate = useCallback(() => {
    useChatStore.getState().regenerate();
  }, []);

  // Select a session from the sidebar. Sessions are fetched for currentAgent
  // (including sessions where currentAgent is a participant but not creator),
  // so we always select for the current agent — never switch agents.
  const handleSelectSession = useCallback((session: { id: string; agent_id: string }) => {
    useChatStore.getState().selectSession(session.id);
    setSheetOpen(false);
  }, []);

  // Switch agent (including Group Chat virtual agent).
  // Override-state fix: set overrideUrlSession = null synchronously so
  // effectiveUrlSessionId is null in the same render as setCurrentAgent — the
  // cross-agent resolver sees !effectiveUrlSessionId and returns early before
  // useSearchParams can update from the now-stale ?s= param.
  // window.history.replaceState clears the physical URL so a hard reload won't
  // carry the previous agent's session ID into the resolver.
  const switchAgent = useCallback((target: string) => {
    restoredAgents.current.delete(target);
    setOverrideUrlSession(null);
    window.history.replaceState(null, "", window.location.pathname);
    useChatStore.getState().setCurrentAgent(target);
  }, []);

  const handleClearError = useCallback(() => {
    useChatStore.getState().clearError();
  }, []);

  // Global keyboard shortcuts (via useHotkey hook)

  // "/" — focus composer (from anywhere except inputs)
  useHotkey("/", (e) => {
    e.preventDefault();
    const input = document.querySelector<HTMLTextAreaElement>('[data-composer-input] textarea');
    input?.focus();
  });

  // Escape — blur active element (works even in inputs)
  useHotkey("Escape", () => {
    (document.activeElement as HTMLElement)?.blur();
  }, { allowInInput: true });

  // Ctrl/Cmd+Shift+N — new chat
  useHotkey("n", (e) => {
    e.preventDefault();
    handleNewChat();
  }, { ctrlOrMeta: true, shift: true });

  // Ctrl/Cmd+K — focus chat input (global scope)
  useHotkey("k", (e) => {
    e.preventDefault();
    const input = document.querySelector<HTMLTextAreaElement>('[data-composer-input] textarea');
    input?.focus();
  }, { ctrlOrMeta: true });

  // Agent selector component (reused in desktop header and mobile)
  const agentSelector = (
    <Select value={currentAgent} onValueChange={switchAgent} aria-label={t("chat.switch_agent")}>
      <SelectTrigger size="sm" className="w-full min-w-0 md:w-auto md:min-w-48 md:max-w-80 shrink text-xs font-semibold uppercase tracking-wide bg-card/50 border-border">
        <SelectValue />
      </SelectTrigger>
      <SelectContent className="border-border">
        {agents.map((a) => (
          <SelectItem key={a} value={a}>
            {a}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );

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

  // ── Session sidebar ──
  const sessionList = (
    <div className="flex h-full flex-col bg-sidebar">
      <TaskPlanPanel agentName={currentAgent} isStreaming={isStreaming} />
      <div className="flex items-center justify-between px-3 py-3 md:px-5 md:py-5 border-b border-border/50">
        <div className="flex flex-col gap-1">
          <span className="text-sm font-display font-semibold text-foreground">
            {t("chat.sessions")}
          </span>
          <span className="text-xs text-muted-foreground-subtle">
            {t("chat.sessions_count", { count: sessionsTotal })}
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
                  : "text-muted-foreground/60 hover:text-destructive hover:bg-destructive/10"
              }`}
              onClick={handleDeleteSessions}
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
            onClick={handleNewChat}
          >
            <Plus className="mr-1.5 h-4 w-4" /> {t("chat.new")}
          </Button>
        </div>
      </div>

      <div className="shrink-0 px-3 py-2 border-b border-border/30">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground/50" />
          <Input
            value={sessionFilter}
            onChange={(e) => setSessionFilter(e.target.value)}
            placeholder={t("chat.search_sessions")}
            className="h-8 pl-8 text-xs bg-muted/30 border-border/50 placeholder:text-muted-foreground/30"
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
              components={{ List: VirtuosoList, Item: VirtuosoListItem }}
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
                      onClick={() => handleSelectSession(s)}
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
                                : "text-muted-foreground/60 group-hover:text-muted-foreground"
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
                          className="w-full truncate text-sm bg-transparent border-b border-primary outline-none focus-visible:ring-1 focus-visible:ring-ring text-foreground placeholder:text-muted-foreground/50"
                          placeholder={t("chat.rename_session")}
                        />
                      ) : (
                        <>
                          <p
                            className={`text-sm transition-colors break-words line-clamp-2 ${
                              activeSessionId === s.id
                                ? "text-foreground"
                                : "text-muted-foreground/60 group-hover:text-muted-foreground/60"
                            } ${!s.title && !s.user_id ? "italic text-muted-foreground/50" : ""}`}
                          >
                            {displayTitle}
                            {s.segment_count != null && s.segment_count > 1 && (
                              <span className="ml-1.5 text-xs text-muted-foreground/50 tabular-nums not-italic whitespace-nowrap">
                                ◈{s.segment_count}
                              </span>
                            )}
                          </p>
                          {s.parent_session_id && (
                            <ParentBadge
                              parentTitle={
                                sessionsData?.sessions?.find((p) => p.id === s.parent_session_id)?.title ?? null
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
                          className="text-muted-foreground/50 hover:text-foreground"
                          title={t("chat.rename_hint")}
                        >
                          <Pencil className="h-4 w-4" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          onClick={(e) => handleShareSession(e, s.id)}
                          disabled={sharingSessionId === s.id}
                          className="text-muted-foreground/50 hover:text-foreground"
                          title={t("chat.share_session")}
                        >
                          <Share2 className="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          onClick={(e) => handleDeleteSession(e, s.id)}
                          disabled={deletingSessionId === s.id}
                          className="text-muted-foreground/50 hover:bg-destructive/10 hover:text-destructive"
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

  // ── Main layout ──
  return (
    <ChatRuntimeProvider key={currentAgent}>
    <div className="flex h-full flex-col lg:flex-row bg-background overflow-hidden">
      <h1 className="sr-only">{t("chat.title")}</h1>
      {/* Desktop sidebar — visible only at lg+ */}
      <aside className="hidden w-70 shrink-0 flex-col border-r border-border lg:flex" aria-label={t("chat.session_list")}>
        {sessionList}
      </aside>

      {/* Chat area */}
      <div className="flex min-w-0 flex-1 flex-col relative h-full">
        {/* Desktop header */}
        <div className="sticky top-0 z-10 hidden h-14 shrink-0 items-center gap-2 lg:gap-4 border-b border-border/50 bg-background/90 backdrop-blur-sm px-4 lg:px-6 lg:flex">
          <div className="flex items-center gap-3 min-w-0 shrink-0">
            {agentSelector}
            <ChatCanvasTabs />
          </div>
          <ContextBar
            tokens={contextTokens}
            model={currentAgentModel}
            modelContextLimit={modelContextLimit}
            cacheReadTokens={cacheReadTokens}
            cacheCreationTokens={cacheCreationTokens}
            reasoningTokens={reasoningTokens}
            isGenerating={isStreaming}
          />
          {/* HISTORY / Return to live badge removed — confusing for users during agent switch */}
          {streamError && (
            <div className="ml-auto flex items-center gap-1 text-destructive/60 shrink-0">
              <div className="h-3 w-3 rounded-full bg-destructive/50 animate-pulse" />
              <span className="text-3xs font-mono uppercase tracking-wider">{t("chat.error")}</span>
            </div>
          )}
        </div>

        {/* Mobile/tablet floating actions — visible below lg */}
        <div className="sticky top-0 z-20 flex shrink-0 items-center gap-0.5 px-1.5 py-1 sm:gap-1 sm:px-2 bg-background/95 backdrop-blur-md border-b border-border/30 lg:hidden overflow-hidden">
          <SidebarTrigger className="h-9 w-9 text-foreground active:scale-90 transition-transform md:hidden shrink-0" />
          <div className="flex min-w-0 flex-1 items-center overflow-hidden">
            {agentSelector}
          </div>
          <ChatCanvasTabs />
          <ContextBar
            compact
            tokens={contextTokens}
            model={currentAgentModel}
            modelContextLimit={modelContextLimit}
            reasoningTokens={reasoningTokens}
            isGenerating={isStreaming}
          />
          <Sheet open={sheetOpen} onOpenChange={setSheetOpen}>
            <SheetTrigger asChild>
              <Button
                variant="outline"
                size="icon"
                className="h-9 w-9 md:h-8 md:w-8 shrink-0 border-primary/30 !bg-primary/10 text-primary shadow-md active:scale-95 transition-all"
                title={t("chat.archive")}
              >
                <Clock className="h-5 w-5 md:h-4 md:w-4" />
              </Button>
            </SheetTrigger>
            <SheetContent
              side="left"
              showCloseButton={false}
              className="w-[85dvw] border-r border-border bg-sidebar p-0"
            >
              <SheetTitle className="sr-only">{t("chat.sessions")}</SheetTitle>
              {sessionList}
            </SheetContent>
          </Sheet>
          <Button
            variant="outline"
            size="icon"
            className="h-9 w-9 md:h-8 md:w-8 shrink-0 border-primary/30 !bg-primary/10 text-primary shadow-md active:scale-95 transition-all"
            onClick={handleNewChat}
            title={t("chat.new")}
          >
            <Plus className="h-5 w-5 md:h-4 md:w-4" />
          </Button>
        </div>

        {/* Messages + Composer */}
        {/* Tab content: Chat or Canvas */}
        <ChatCanvasContent
          key={currentAgent}
          currentAgent={currentAgent}
          activeSessionId={activeSessionId}
          streamError={streamError}
          isReadOnly={isReadOnly}
          activeSession={activeSession}
          onClearError={handleClearError}
          onRetry={() => { handleClearError(); handleRegenerate(); }}
        />
      </div>

    </div>

      <AlertDialog open={deleteAllOpen} onOpenChange={(o) => { if (!o) setDeleteAllOpen(false); }}>
        <AlertDialogContent className="rounded-xl border-border bg-card">
          <AlertDialogHeader>
            <AlertDialogTitle className="text-base font-bold text-destructive">{t("chat.delete_all_confirm_title", { agent: currentAgent })}</AlertDialogTitle>
            <AlertDialogDescription className="text-sm text-muted-foreground">
              {t("chat.delete_all_confirm_description", { count: sessionsTotal, agent: currentAgent })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction onClick={handleDeleteAll} disabled={deletingAll} className="bg-destructive text-destructive-foreground hover:bg-destructive/90">
              {t("chat.delete_all")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

    </ChatRuntimeProvider>
  );
}

// ── Chat / Canvas tab switching ────────────────────────────────────────────

function ChatCanvasTabs({ className }: { className?: string }) {
  const { t } = useTranslation();
  const panelOpen = useCanvasStore((s) => s.panelOpen);
  const setPanelOpen = useCanvasStore((s) => s.setPanelOpen);

  return (
    <Tabs value={panelOpen ? "canvas" : "chat"} onValueChange={(v) => setPanelOpen(v === "canvas")}>
      <TabsList className={cn("h-8 w-fit", className)}>
        <TabsTrigger value="chat" className="w-fit gap-1.5 px-2.5 text-xs font-medium">
          <MessageSquare className="size-4" />
          <span className="hidden lg:inline">{t("nav.chat")}</span>
        </TabsTrigger>
        <TabsTrigger value="canvas" className="w-fit gap-1.5 px-2.5 text-xs font-medium">
          <PanelRight className="size-4" />
          <span className="hidden lg:inline">{t("nav.canvas")}</span>
        </TabsTrigger>
      </TabsList>
    </Tabs>
  );
}

function ChatCanvasContent({
  currentAgent,
  activeSessionId,
  streamError,
  isReadOnly,
  activeSession,
  onClearError,
  onRetry,
}: {
  currentAgent: string;
  activeSessionId: string | null;
  streamError: string | null;
  isReadOnly: boolean;
  activeSession?: import("@/types/api").SessionRow;
  onClearError: () => void;
  onRetry: () => void;
}) {
  const panelOpen = useCanvasStore((s) => s.panelOpen);

  if (panelOpen) {
    return (
      <div className="flex flex-1 flex-col min-h-0">
        <CanvasPanel agent={currentAgent} />
      </div>
    );
  }

  return (
    <div className="flex flex-1 flex-col min-h-0">
      {activeSessionId && (
        <CompactChainBanner
          key={`banner-${currentAgent}-${activeSessionId}`}
          activeSessionId={activeSessionId}
          onNavigate={(sid) => useChatStore.getState().selectSession(sid, currentAgent)}
        />
      )}
      <ChatThread
        key={`thread-${currentAgent}`}
        agent={currentAgent}
        streamError={streamError}
        isReadOnly={isReadOnly}
        activeSession={activeSession}
        onClearError={onClearError}
        onRetry={onRetry}
      />
    </div>
  );
}
