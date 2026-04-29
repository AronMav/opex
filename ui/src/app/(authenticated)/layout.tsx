"use client";

import { useEffect, useState, useRef } from "react";
import { useRouter } from "next/navigation";
import { useAuthStore } from "@/stores/auth-store";
import { apiGet, apiPost } from "@/lib/api";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";
import { useWsStore } from "@/stores/ws-store";
import { useChatStore } from "@/stores/chat-store";
import { useWsSubscription } from "@/hooks/use-ws-subscription";
import { useTranslation } from "@/hooks/use-translation";
import { toast } from "sonner";
import { SidebarProvider, SidebarInset, SidebarTrigger } from "@/components/ui/sidebar";
import { AppSidebar } from "@/components/app-sidebar";
import { QueryProvider } from "@/providers/query-provider";
import { Bot } from "lucide-react";

export default function AuthenticatedLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const router = useRouter();
  const token = useAuthStore((s) => s.token);
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  const restore = useAuthStore((s) => s.restore);
  const connectWs = useWsStore((s) => s.connect);
  const disconnectWs = useWsStore((s) => s.disconnect);
  const connected = useWsStore((s) => s.connected);
  const { t } = useTranslation();
  const [ready, setReady] = useState(false);
  const restoredRef = useRef(false);

  useEffect(() => {
    const init = async (authenticated: boolean) => {
      if (!authenticated) {
        router.replace("/login");
        return;
      }
      // Check if setup is needed
      try {
        const res = await apiGet<{ needs_setup: boolean }>("/api/setup/status");
        if (res.needs_setup) {
          router.replace("/setup");
          return;
        }
      } catch (e) {
        console.warn("[layout] setup check failed, proceeding:", e);
      }
      setReady(true);
    };

    if (isAuthenticated) {
      init(true);
      return;
    }
    if (!restoredRef.current) {
      restoredRef.current = true;
      restore().then((ok) => {
        if (ok) {
          init(true);
          return;
        }
        // restore() returns false in two cases:
        //  (a) 401 — token confirmed invalid: logout() already cleared the token
        //  (b) network error — token is still present, server was transiently unavailable
        // Only redirect for (a). For (b) proceed optimistically; individual API
        // calls will handle 401 if the token is actually invalid.
        const tokenStillExists = !!useAuthStore.getState().token;
        init(tokenStillExists ? true : false);
      });
    }
  }, [isAuthenticated, restore, router]);

  useEffect(() => {
    if (token) {
      connectWs(token);
      return () => disconnectWs();
    }
  }, [token, connectWs, disconnectWs]);

  // Global handlers: persist during SPA navigation (unlike per-page useWsSubscription)
  useWsSubscription("agent_processing", (msg) => {
    if (!msg.agent) return;
    const store = useChatStore.getState();
    if (msg.status === "start") {
      store.setThinking(msg.agent, msg.session_id ?? null);
      // Refresh thread list so the new session appears in the sidebar immediately
      queryClient.invalidateQueries({ queryKey: qk.sessions(msg.agent) });
    } else {
      store.setThinking(msg.agent, null);
      queryClient.invalidateQueries({ queryKey: qk.sessions(msg.agent) });
    }
  });

  // Approval requests must be visible on ANY page — agent can hang waiting for approval
  useWsSubscription("approval_requested", (msg) => {
    const { approval_id: approvalId, agent: agentName, tool: toolName } = msg;
    toast(`${agentName}: ${toolName}`, {
      description: t("chat.approval_description", { tool: toolName, agent: agentName }),
      duration: 30000,
      action: {
        label: t("chat.approve"),
        onClick: () => {
          apiPost(`/api/approvals/${approvalId}/resolve`, { status: "approved", resolved_by: "ui" }).catch(() => {
            toast.error(t("chat.approval_error"));
          });
        },
      },
      cancel: {
        label: t("chat.reject"),
        onClick: () => {
          apiPost(`/api/approvals/${approvalId}/resolve`, { status: "rejected", resolved_by: "ui" }).catch(() => {
            toast.error(t("chat.approval_error"));
          });
        },
      },
    });
  });

  if (!ready) {
    return (
      <div className="flex h-dvh items-center justify-center bg-background">
        <div className="flex flex-col items-center gap-4">
          <div className="h-10 w-10 animate-spin rounded-full border-2 border-primary border-t-transparent" />
          <span className="text-sm text-muted-foreground">{t("common.loading")}</span>
        </div>
      </div>
    );
  }

  return (
    <QueryProvider>
    <SidebarProvider>
      <AppSidebar />
      <SidebarInset className="flex flex-col h-[100dvh] min-h-0 bg-transparent relative">
        {/* Unified Mobile Header */}
        <div className="sticky top-0 z-30 flex h-14 shrink-0 items-center justify-between border-b border-border bg-background px-3 md:hidden">
          <div className="flex items-center gap-2">
            <SidebarTrigger className="h-9 w-9 text-foreground active:scale-90 transition-transform" />
            <div className="flex items-center gap-2 pr-2 border-r border-border/20">
              <Bot className="h-4 w-4 text-primary" />
              <span className="font-display text-sm font-black tracking-wide uppercase text-foreground/90">HydeClaw</span>
            </div>

            <div id="mobile-page-actions" className="flex items-center gap-1">
              {/* Portal target for page-specific buttons */}
            </div>
          </div>

          <div className={`flex items-center gap-1.5 rounded-full px-2 py-0.5 border ${connected ? "border-success/20 bg-success/5 text-success" : "border-destructive/20 bg-destructive/5 text-destructive"}`}>
            <span className={`h-1.5 w-1.5 rounded-full ${connected ? "bg-success" : "bg-destructive"}`} />
            <span className="font-mono text-[9px] font-bold uppercase tracking-tight leading-none">
              {connected ? t("common.live") : t("common.offline")}
            </span>
          </div>
        </div>

        <main className="flex-1 flex flex-col min-h-0 min-w-0 overflow-y-auto">
          {children}
        </main>
      </SidebarInset>
    </SidebarProvider>
    </QueryProvider>
  );
}
