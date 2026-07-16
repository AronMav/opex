"use client";

import React, { useEffect, useState, useCallback, useMemo } from "react";
import { useShallow } from "zustand/react/shallow";
import { useAuthStore } from "@/stores/auth-store";
import {
  useChatStore,
  isActivePhase,
} from "@/stores/chat-store";
import { useHotkey } from "@/hooks/use-hotkey";
import { ChatRuntimeProvider } from "@/providers/assistant-runtime";
import { useTranslation } from "@/hooks/use-translation";
import { cn } from "@/lib/utils";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
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
  PanelRight,
  MessageSquare,
} from "lucide-react";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ChatThread } from "./ChatThread";
import { ContextBar } from "./ContextBar";
import { CanvasPanel } from "./CanvasPanel";
import { CompactChainBanner } from "@/components/chat/CompactChainBanner";
import { useCanvasStore } from "@/stores/canvas-store";
import { useSessions, useAgents, qk } from "@/lib/queries";
import { useAgentTextModel } from "@/hooks/use-profiles";
import { queryClient } from "@/lib/query-client";
import { shareSession } from "@/lib/api";
import type { SessionRow } from "@/types/api";
import { useSessionRestore } from "./hooks/use-session-restore";
import { useChatWs } from "./hooks/use-chat-ws";
import { SessionSidebar } from "./SessionSidebar";

const EMPTY_SESSIONS: SessionRow[] = [];
const EMPTY_ACTIVE: string[] = [];

export default function ChatPage() {
  const { t } = useTranslation();
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

  // Chat-page WebSocket subscriptions (session_updated, agent_processing,
  // file_job_progress, agent_joined). Extracted verbatim.
  useChatWs();

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

  // Ctrl/Cmd+K is owned by the search palette (SearchPalette.tsx) — the
  // legacy focus-composer binding was removed here; "/" already covers
  // focusing the composer from anywhere.

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

  // ── Session sidebar props (state + handlers stay in page.tsx so the desktop
  // pane and mobile Sheet share one state instance; SessionSidebar is presentational) ──
  const sidebarProps = {
    currentAgent,
    isStreaming,
    sessions,
    sessionsData,
    sessionsLoading,
    sessionsTotal,
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
    onNewChat: handleNewChat,
    onSelectSession: handleSelectSession,
    onDeleteSessions: handleDeleteSessions,
    onDeleteSession: handleDeleteSession,
    onShareSession: handleShareSession,
    toggleSessionSelection,
  };


  // ── Main layout ──
  return (
    <ChatRuntimeProvider key={currentAgent}>
    <div className="flex h-full flex-col lg:flex-row bg-background overflow-hidden">
      <h1 className="sr-only">{t("chat.title")}</h1>
      {/* Desktop sidebar — visible only at lg+ */}
      <aside className="hidden w-70 shrink-0 flex-col border-r border-border lg:flex" aria-label={t("chat.session_list")}>
        <SessionSidebar {...sidebarProps} />
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
              <SessionSidebar {...sidebarProps} />
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
