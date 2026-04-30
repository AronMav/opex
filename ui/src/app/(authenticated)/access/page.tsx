"use client";

import { useEffect, useState, useCallback } from "react";
import { apiGet, apiPost, apiDelete, apiPut } from "@/lib/api";
import { useAgents, qk } from "@/lib/queries";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Badge } from "@/components/ui/badge";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { formatDate } from "@/lib/format";
import { useTranslation } from "@/hooks/use-translation";
import { ErrorBanner } from "@/components/ui/error-banner";
import {
  ShieldCheck,
  UserX,
  UserCheck,
  ShieldAlert,
  RefreshCw,
  ChevronDown,
} from "lucide-react";
import { toast } from "sonner";
import type { AgentDetail } from "@/types/api";

interface PendingPairing {
  code: string;
  channel_user_id: string;
  display_name: string | null;
  created_at: string;
}

interface AllowedUser {
  channel_user_id: string;
  display_name: string | null;
  approved_at: string;
}

interface AccessSettings {
  enabled: boolean;
  mode: string;
  owner_id: string;
}

export default function AccessPage() {
  const { t, locale } = useTranslation();
  const { data: agentInfos = [], isLoading: agentsLoading, error: agentsError, refetch } = useAgents();
  const qc = useQueryClient();

  const agents = agentInfos.map((a) => a.name);

  const [agentDetails, setAgentDetails] = useState<Record<string, AgentDetail>>({});
  const [accessSettings, setAccessSettings] = useState<Record<string, AccessSettings>>({});
  const [pending, setPending] = useState<Record<string, PendingPairing[]>>({});
  const [users, setUsers] = useState<Record<string, AllowedUser[]>>({});
  const [actionError, setActionError] = useState("");
  const [removeTarget, setRemoveTarget] = useState<{ agent: string; userId: string; name: string } | null>(null);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [savingAgents, setSavingAgents] = useState<Set<string>>(new Set());

  const error = agentsError ? `${agentsError}` : actionError;

  const toggleExpand = (agent: string) =>
    setExpanded((prev) => ({ ...prev, [agent]: !prev[agent] }));

  const loadAgentDetails = useCallback(async (agentNames: string[]) => {
    for (const agent of agentNames) {
      try {
        const detail = await apiGet<AgentDetail>(`/api/agents/${agent}`);
        setAgentDetails((prev) => ({ ...prev, [agent]: detail }));
        setAccessSettings((prev) => ({
          ...prev,
          [agent]: {
            enabled: !!detail.access,
            mode: detail.access?.mode ?? "open",
            owner_id: detail.access?.owner_id ?? "",
          },
        }));
      } catch (e) {
        console.warn(`[access] failed to load agent ${agent}:`, e);
      }
    }
  }, []);

  const loadAccess = useCallback(async (agentNames: string[]) => {
    for (const agent of agentNames) {
      try {
        const [p, u] = await Promise.all([
          apiGet<{ pending: PendingPairing[] }>(`/api/access/${agent}/pending`),
          apiGet<{ users: AllowedUser[] }>(`/api/access/${agent}/users`),
        ]);
        setPending((prev) => ({ ...prev, [agent]: p.pending || [] }));
        setUsers((prev) => ({ ...prev, [agent]: u.users || [] }));
      } catch (e) {
        console.warn(`[access] failed to load access for ${agent}:`, e);
      }
    }
  }, []);

  useEffect(() => {
    if (agents.length === 0) return;
    loadAgentDetails(agents);
    loadAccess(agents);
  }, [agents.join(",")]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (agents.length === 0) return;
    const id = setInterval(() => loadAccess(agents), 5000);
    return () => clearInterval(id);
  }, [agents.join(","), loadAccess]); // eslint-disable-line react-hooks/exhaustive-deps

  const saveAccessSettings = async (agent: string) => {
    const detail = agentDetails[agent];
    const settings = accessSettings[agent];
    if (!detail || !settings) return;

    setSavingAgents(prev => new Set(prev).add(agent));
    setActionError("");
    try {
      await apiPut(`/api/agents/${agent}`, {
        ...detail,
        access: settings.enabled
          ? { mode: settings.mode, owner_id: settings.owner_id || null }
          : null,
      });
      qc.invalidateQueries({ queryKey: qk.agents });
      await Promise.all([loadAgentDetails([agent]), loadAccess([agent])]);
      toast.success(`${agent}: ${t("access.settings_saved")}`);
    } catch (e) {
      toast.error(t("access.save_failed", { error: `${e}` }));
    } finally {
      setSavingAgents(prev => { const s = new Set(prev); s.delete(agent); return s; });
    }
  };

  const updAccessSettings = (agent: string, patch: Partial<AccessSettings>) => {
    setAccessSettings((prev) => ({
      ...prev,
      [agent]: { ...(prev[agent] ?? { enabled: false, mode: "open", owner_id: "" }), ...patch },
    }));
  };

  const approve = async (agent: string, code: string) => {
    try {
      await apiPost(`/api/access/${agent}/approve/${code}`, {});
      await loadAccess(agents);
    } catch (e) {
      toast.error(`${e}`);
    }
  };

  const reject = async (agent: string, code: string) => {
    try {
      await apiPost(`/api/access/${agent}/reject/${code}`, {});
      await loadAccess(agents);
    } catch (e) {
      toast.error(`${e}`);
    }
  };

  const doRemove = async () => {
    if (!removeTarget) return;
    try {
      await apiDelete(`/api/access/${removeTarget.agent}/users/${removeTarget.userId}`);
      setRemoveTarget(null);
      await loadAccess(agents);
    } catch (e) {
      toast.error(`Failed to revoke: ${e}`);
    }
  };

  const totalPending = Object.values(pending).reduce((sum, arr) => sum + arr.length, 0);

  return (
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
      {/* Header */}
      <div className="mb-8 flex flex-col gap-4 md:flex-row md:items-center md:justify-between">
        <div>
          <h2 className="font-display text-lg font-bold tracking-tight text-foreground">{t("access.title")}</h2>
          <p className="text-sm text-muted-foreground mt-1">{t("access.subtitle")}</p>
        </div>
        <div className="flex items-center gap-2">
          {totalPending > 0 && (
            <Badge variant="outline" className="text-xs border-warning/50 text-warning bg-warning/5 gap-1">
              <ShieldAlert className="h-3 w-3" />
              {totalPending} {t("access.pending_approvals")}
            </Badge>
          )}
          <Button variant="outline" size="sm" onClick={() => { refetch(); loadAccess(agents); }} disabled={agentsLoading}>
            <RefreshCw className={`mr-2 h-4 w-4 ${agentsLoading ? "animate-spin" : ""}`} />
            {t("common.refresh")}
          </Button>
        </div>
      </div>

      {error && <ErrorBanner error={error} />}

      {!agentsLoading && agents.length === 0 ? (
        <div className="flex h-40 items-center justify-center rounded-xl border border-dashed border-border bg-muted/10">
          <p className="font-mono text-sm text-muted-foreground/40 uppercase tracking-wider">{t("access.no_agents")}</p>
        </div>
      ) : (
        <div className="space-y-3">
          {agents.map((agent) => {
            const settings = accessSettings[agent] ?? { enabled: false, mode: "open", owner_id: "" };
            const agentPending = pending[agent] ?? [];
            const agentUsers = users[agent] ?? [];
            const isExpanded = expanded[agent] ?? false;
            const isSaving = savingAgents.has(agent);

            return (
              <div key={agent} className="rounded-xl border border-border/60 bg-card/50 overflow-hidden">
                {/* Compact header row */}
                <button
                  className="w-full flex items-center gap-3 p-4 hover:bg-muted/30 transition-colors text-left"
                  onClick={() => toggleExpand(agent)}
                >
                  <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary/10 border border-primary/20 shrink-0">
                    <ShieldCheck className="h-4 w-4 text-primary" />
                  </div>
                  <span className="font-mono text-sm font-bold tracking-tight text-foreground truncate flex-1">{agent}</span>

                  {/* Status pills */}
                  <div className="flex items-center gap-2 shrink-0">
                    {agentPending.length > 0 && (
                      <Badge variant="outline" className="text-[10px] border-warning/50 text-warning bg-warning/5 px-1.5 py-0">
                        {agentPending.length} {t("access.pending")}
                      </Badge>
                    )}
                    <Badge
                      variant="secondary"
                      className={`text-[10px] px-1.5 py-0 ${
                        settings.enabled
                          ? settings.mode === "restricted"
                            ? "text-amber-600 bg-amber-500/10 border border-amber-500/20"
                            : "text-green-600 bg-green-500/10 border border-green-500/20"
                          : "text-muted-foreground"
                      }`}
                    >
                      {settings.enabled ? (settings.mode === "restricted" ? t("access.restricted") : t("access.open")) : t("access.disabled")}
                    </Badge>
                    <span className="text-[11px] text-muted-foreground/50 font-mono">{t("access.users_count", { count: agentUsers.length })}</span>
                    <ChevronDown className={`h-4 w-4 text-muted-foreground/40 transition-transform ${isExpanded ? "rotate-180" : ""}`} />
                  </div>
                </button>

                {/* Expandable content */}
                {isExpanded && (
                  <div className="border-t border-border/40 p-4 space-y-4 animate-in fade-in slide-in-from-top-1 duration-150">
                    {/* Settings row */}
                    <div className="flex flex-wrap items-center gap-3">
                      <div className="flex items-center gap-2">
                        <span className="text-xs text-muted-foreground">{t("access.enable_access_control")}</span>
                        <Switch
                          checked={settings.enabled}
                          onCheckedChange={(v) => updAccessSettings(agent, { enabled: v })}
                          className="data-[state=checked]:bg-primary scale-90"
                        />
                      </div>
                      {settings.enabled && (
                        <>
                          <div
                            role="radiogroup"
                            aria-label={t("access.mode_label")}
                            className="flex gap-0.5 p-0.5 bg-muted/40 rounded-md border border-border"
                          >
                            {["open", "restricted"].map((mode) => (
                              <button
                                key={mode}
                                role="radio"
                                aria-checked={settings.mode === mode}
                                className={`px-2.5 py-1 text-[11px] font-medium rounded transition-all ${
                                  settings.mode === mode
                                    ? "bg-primary text-primary-foreground shadow-sm"
                                    : "text-muted-foreground hover:text-foreground"
                                }`}
                                onClick={() => updAccessSettings(agent, { mode })}
                              >
                                {mode === "restricted" ? t("access.restricted") : t("access.open")}
                              </button>
                            ))}
                          </div>
                          <Input
                            value={settings.owner_id}
                            placeholder="owner_id"
                            className="bg-background font-mono text-xs h-7 w-40"
                            onChange={(e) => updAccessSettings(agent, { owner_id: e.target.value })}
                          />
                        </>
                      )}
                      <Button size="sm" onClick={() => saveAccessSettings(agent)} disabled={isSaving} className="h-7 text-xs font-semibold ml-auto">
                        {isSaving ? t("common.saving") : t("common.save")}
                      </Button>
                    </div>

                    {/* Pending pairings */}
                    {agentPending.length > 0 && (
                      <div className="space-y-2">
                        <div className="flex items-center gap-2">
                          <ShieldAlert className="h-3.5 w-3.5 text-warning" />
                          <span className="text-xs font-semibold text-warning">{t("access.pending_approvals")}</span>
                        </div>
                        {agentPending.map((p) => (
                          <div key={p.code} className="flex items-center justify-between gap-3 rounded-lg border border-warning/20 bg-warning/5 px-3 py-2">
                            <div className="flex items-center gap-2 min-w-0">
                              <span className="font-semibold text-xs truncate">{p.display_name || t("access.unknown_user")}</span>
                              <Badge variant="outline" className="font-mono text-[10px] px-1 py-0 bg-background shrink-0">{p.code}</Badge>
                              <span className="font-mono text-[10px] text-muted-foreground/50 truncate hidden sm:inline">{p.channel_user_id}</span>
                            </div>
                            <div className="flex gap-1.5 shrink-0">
                              <Button size="sm" onClick={() => approve(agent, p.code)} className="h-6 px-2 text-[10px] bg-success text-success-foreground hover:bg-success/90">
                                {t("access.approve")}
                              </Button>
                              <Button variant="outline" size="sm" onClick={() => reject(agent, p.code)} className="h-6 px-2 text-[10px] border-destructive/40 text-destructive hover:bg-destructive/10">
                                {t("access.reject")}
                              </Button>
                            </div>
                          </div>
                        ))}
                      </div>
                    )}

                    {/* Authorized users */}
                    <div className="space-y-2">
                      <div className="flex items-center gap-2">
                        <UserCheck className="h-3.5 w-3.5 text-primary" />
                        <span className="text-xs font-semibold text-foreground/70">{t("access.authorized_users")}</span>
                        <span className="text-[10px] text-muted-foreground/40 font-mono">{agentUsers.length}</span>
                      </div>
                      {agentUsers.length === 0 ? (
                        <div className="flex h-12 items-center justify-center rounded-lg border border-dashed border-border/50 bg-muted/5">
                          <span className="text-xs text-muted-foreground/40">{t("access.no_authorized_users")}</span>
                        </div>
                      ) : (
                        <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
                          {agentUsers.map((u) => (
                            <div key={u.channel_user_id} className="group flex items-center gap-2.5 rounded-lg border border-border/50 bg-card/30 px-3 py-2 transition-colors hover:bg-card/60">
                              <div className="flex flex-col min-w-0 flex-1">
                                <span className="font-semibold text-xs truncate">{u.display_name || "—"}</span>
                                <span className="font-mono text-[10px] text-muted-foreground/50 truncate">{u.channel_user_id}</span>
                              </div>
                              <span className="text-[9px] text-muted-foreground/30 font-mono hidden sm:block shrink-0">
                                {t("access.granted_at", { date: formatDate(u.approved_at, locale) })}
                              </span>
                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-6 w-6 text-muted-foreground/30 hover:text-destructive hover:bg-destructive/10 opacity-0 group-hover:opacity-100 transition-opacity shrink-0"
                                onClick={() => setRemoveTarget({ agent, userId: u.channel_user_id, name: u.display_name || u.channel_user_id })}
                              >
                                <UserX className="h-3.5 w-3.5" />
                              </Button>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}

      <ConfirmDialog
        open={!!removeTarget}
        onClose={() => setRemoveTarget(null)}
        onConfirm={doRemove}
        title={t("access.revoke_title")}
        description={t("access.revoke_description", { name: removeTarget?.name ?? "" })}
        confirmLabel={t("access.revoke")}
      />
    </div>
  );
}
