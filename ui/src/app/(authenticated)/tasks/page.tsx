"use client";

import { useState, useCallback, useEffect } from "react";
import { apiGet } from "@/lib/api";
import {
  useCronJobs,
  useCronRuns,
  useCreateCronJob,
  useUpdateCronJob,
  useDeleteCronJob,
  useRunCronJob,
} from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import { ErrorBanner } from "@/components/ui/error-banner";
import { PageHeader } from "@/components/ui/page-header";
import { Button } from "@/components/ui/button";
import { CircularLoader } from "@/components/ui/loader";
import { Skeleton } from "@/components/ui/skeleton";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { PageContainer } from "@/components/ui/page-container";
import { StatusBadge } from "@/components/ui/status-badge";
import { StatusDot } from "@/components/ui/status-dot";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { CronSchedulePicker } from "@/components/ui/cron-schedule-picker";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Collapsible, CollapsibleTrigger, CollapsibleContent } from "@/components/ui/collapsible";
import { useAuthStore } from "@/stores/auth-store";
import { useWsSubscription } from "@/hooks/use-ws-subscription";
import { useQueryClient } from "@tanstack/react-query";
import { EmptyState } from "@/components/ui/empty-state";
import { Field } from "@/components/ui/field";
import { Clock, Play, Power, PowerOff, Trash2, Edit3, Plus, ChevronDown, ChevronUp, History } from "lucide-react";
import type { CronJob, ChannelRow } from "@/types/api";

const emptyJob = { name: "", agent: "", cron: "", timezone: "Europe/Samara", task: "", silent: false, announce_to: null as { channel: string; chat_id: number } | null, jitter_secs: 0, run_once: false, run_at: null as string | null, run_at_local: "" };

import { isValidCron } from "@/lib/cron";

export default function CronPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data: jobs = [], isLoading, error } = useCronJobs();
  const [actionError, setActionError] = useState("");
  const [formOpen, setFormOpen] = useState(false);
  const [editId, setEditId] = useState<string | null>(null);
  const [form, setForm] = useState(emptyJob);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const agents = useAuthStore((s) => s.agents);
  const [expandedJob, setExpandedJob] = useState<string | null>(null);
  const [agentChannels, setAgentChannels] = useState<ChannelRow[]>([]);
  const [toolPolicyAllow, setToolPolicyAllow] = useState("");
  const [toolPolicyDeny, setToolPolicyDeny] = useState("");

  const createJob = useCreateCronJob();
  const updateJob = useUpdateCronJob();
  const deleteJob = useDeleteCronJob();
  const runJob = useRunCronJob();

  const mutating = createJob.isPending || updateJob.isPending || deleteJob.isPending || runJob.isPending;

  const { data: runs = [], isLoading: runsLoading } = useCronRuns(expandedJob);

  const toggleHistory = useCallback((jobId: string) => {
    setExpandedJob((prev) => (prev === jobId ? null : jobId));
  }, []);

  // Auto-refresh when a cron job completes
  useWsSubscription("session_updated", useCallback(() => {
    queryClient.invalidateQueries({ queryKey: ["cron"] });
  }, [queryClient]));

  // Load channels for the selected agent (for announce_to selector)
  useEffect(() => {
    if (!form.agent || !formOpen) return;
    apiGet<ChannelRow[]>(`/api/agents/${form.agent}/channels`)
      .then((channels) => setAgentChannels(Array.isArray(channels) ? channels : []))
      .catch((e) => { console.warn("[tasks] channels load failed:", e); setAgentChannels([]); });
  }, [form.agent, formOpen]);

  const openCreate = () => {
    setEditId(null);
    setForm({ ...emptyJob, agent: agents[0] || "" });
    setToolPolicyAllow("");
    setToolPolicyDeny("");
    setFormOpen(true);
  };

  const openEdit = (j: CronJob) => {
    setEditId(j.id);
    setForm({ name: j.name, agent: j.agent, cron: j.cron, timezone: j.timezone, task: j.task, silent: j.silent, announce_to: j.announce_to ?? null, jitter_secs: j.jitter_secs ?? 0, run_once: j.run_once, run_at: j.run_at ?? null, run_at_local: j.run_at ? new Date(j.run_at).toISOString().slice(0, 16) : "" });
    setToolPolicyAllow((j.tool_policy?.allow ?? []).join("\n"));
    setToolPolicyDeny((j.tool_policy?.deny ?? []).join("\n"));
    setFormOpen(true);
  };

  const saveJob = useCallback(async () => {
    if (!form.run_once && form.cron.trim() && !isValidCron(form.cron)) {
      setActionError(t("cron.cron_invalid"));
      return;
    }
    setActionError("");
    const allow = toolPolicyAllow.split("\n").map((s) => s.trim()).filter(Boolean);
    const deny = toolPolicyDeny.split("\n").map((s) => s.trim()).filter(Boolean);
    const tool_policy = allow.length > 0 || deny.length > 0 ? { allow, deny } : undefined;
    try {
      if (editId) {
        // `run_at_local` is a UI-only helper, not part of the API payload.
        const { run_at_local: _unused, ...payload } = form;
        void _unused;
        await updateJob.mutateAsync({ id: editId, ...payload, tool_policy });
      } else {
        // `run_at_local` is a UI-only helper, not part of the API payload.
        const { run_at_local: _unused, ...payload } = form;
        void _unused;
        await createJob.mutateAsync({ ...payload, tool_policy });
      }
      setFormOpen(false);
    } catch (e) {
      setActionError(`${e}`);
    }
  }, [form, editId, updateJob, createJob, t, toolPolicyAllow, toolPolicyDeny]);

  const toggleEnabled = useCallback(async (j: CronJob) => {
    setActionError("");
    try {
      await updateJob.mutateAsync({ id: j.id, enabled: !j.enabled });
    } catch (e) {
      setActionError(`${e}`);
    }
  }, [updateJob]);

  const runNow = useCallback(async (id: string) => {
    setActionError("");
    try {
      await runJob.mutateAsync(id);
    } catch (e) {
      setActionError(`${e}`);
    }
  }, [runJob]);

  const doDelete = useCallback(async () => {
    if (!deleteTarget) return;
    setActionError("");
    try {
      await deleteJob.mutateAsync(deleteTarget);
      setDeleteTarget(null);
    } catch (e) {
      setActionError(`${e}`);
    }
  }, [deleteTarget, deleteJob]);

  const errorMessage = error ? `${error}` : actionError;

  return (
    <PageContainer>
        <PageHeader
          title={t("cron.title")}
          description={t("cron.subtitle")}
          actions={
            <Button
              size="lg"
              onClick={openCreate}
              className="w-full md:w-auto gap-2"
            >
              <Plus className="h-4 w-4" /> {t("cron.new_task")}
            </Button>
          }
        />

        {errorMessage && <ErrorBanner error={errorMessage} />}

        {isLoading ? (
          <div className="grid gap-4 md:gap-6">
            {[1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-32 rounded-xl" />
            ))}
          </div>
        ) : jobs.length === 0 ? (
          <EmptyState icon={Clock} text={t("cron.no_tasks")} />
        ) : (
          <div className="grid gap-4 md:gap-6">
            {jobs.map((j) => (
              <Card key={j.id} className={`group relative flex flex-col md:flex-row md:flex-wrap md:items-stretch gap-4 p-5 min-w-0 overflow-hidden transition-all duration-300 ${
                j.enabled
                  ? "hover:shadow-lg"
                  : "opacity-70 hover:opacity-100"
              }`}>
                <div className="flex-1 flex flex-col justify-between min-w-0">
                  <div className="flex flex-col gap-2">
                    <div className="flex items-center gap-3 flex-wrap">
                      <h3 className="font-mono text-base font-bold text-foreground truncate min-w-0">{j.name}</h3>
                      <Badge variant="outline-primary">
                        {j.agent}
                      </Badge>
                      <StatusBadge status={j.enabled ? "active" : "paused"}>
                        {j.enabled ? t("cron.active") : t("cron.paused")}
                      </StatusBadge>
                      {j.silent && (
                        <Badge variant="secondary" size="xs">
                          {t("cron.silent")}
                        </Badge>
                      )}
                      {j.run_once && (
                        <Badge variant="outline-primary" size="xs">
                          {t("tasks.once")}
                        </Badge>
                      )}
                    </div>
                    <div className="flex items-center gap-2 mt-1 flex-wrap min-w-0">
                      <Clock className="h-3.5 w-3.5 text-muted-foreground/60 shrink-0" />
                      <span className="font-mono text-xs text-muted-foreground font-bold tracking-wider truncate max-w-full">{j.cron}</span>
                      <span className="font-mono text-xs text-muted-foreground/60 uppercase tracking-wide bg-muted/50 px-2 py-0.5 rounded border border-border/50 max-w-36 truncate shrink-0">
                        {j.timezone}
                      </span>
                    </div>
                  </div>
                  <div className="mt-4 rounded-lg bg-muted/30 border border-border/30 p-3">
                    <p className="font-mono text-sm leading-relaxed text-foreground/80 line-clamp-2 break-words">
                      {j.task}
                    </p>
                  </div>
                </div>

                <div className="flex flex-col gap-2 border-t border-border/50 pt-4 md:border-t-0 md:pt-0 md:border-l md:pl-4 shrink-0 md:w-36">
                  <div className="grid grid-cols-2 md:grid-cols-1 gap-2">
                    <Button
                      variant={j.enabled ? "outline-warning" : "default"}
                      size="sm"
                      onClick={() => toggleEnabled(j)}
                      disabled={mutating}
                      className="text-xs font-medium"
                    >
                      {j.enabled ? <><PowerOff className="h-4 w-4 mr-1.5" /> {t("cron.pause")}</> : <><Power className="h-4 w-4 mr-1.5" /> {t("cron.enable")}</>}
                    </Button>
                    <Button
                      variant="outline-success"
                      size="sm"
                      onClick={() => runNow(j.id)}
                      disabled={mutating || !j.enabled}
                      className="text-xs font-medium"
                    >
                      <Play className="h-4 w-4 mr-1.5" /> {t("cron.run_now")}
                    </Button>
                  </div>
                  <div className="grid grid-cols-3 gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => openEdit(j)}
                      disabled={mutating}
                      className="text-muted-foreground hover:text-primary hover:bg-primary/10"
                    >
                      <Edit3 className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setDeleteTarget(j.id)}
                      disabled={mutating}
                      className="text-muted-foreground hover:text-destructive hover:bg-destructive/10"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => toggleHistory(j.id)}
                      className="text-xs text-muted-foreground hover:text-foreground"
                    >
                      <History className="h-3.5 w-3.5" />
                      {expandedJob === j.id ? <ChevronUp className="h-4 w-4 ml-0.5" /> : <ChevronDown className="h-4 w-4 ml-0.5" />}
                    </Button>
                  </div>
                </div>

                {expandedJob === j.id && (
                  <div className="w-full border-t border-border/50 pt-4 mt-2">
                    <h4 className="text-xs font-semibold text-muted-foreground mb-3 uppercase tracking-wider">{t("cron.recent_runs")}</h4>
                    {runsLoading ? (
                      <div className="h-16 flex items-center justify-center">
                        <CircularLoader size="sm" />
                      </div>
                    ) : runs.length === 0 ? (
                      <p className="text-xs text-muted-foreground-subtle text-center py-4">{t("cron.no_runs")}</p>
                    ) : (
                      <div className="space-y-2">
                        {runs.map((r) => {
                          const duration = r.finished_at
                            ? Math.round((new Date(r.finished_at).getTime() - new Date(r.started_at).getTime()) / 1000)
                            : null;
                          return (
                            <div key={r.id} className="flex items-start gap-3 p-3 rounded-lg bg-muted/30 border border-border/30 min-w-0 overflow-hidden">
                              <StatusBadge
                                status={r.status === "success" ? "success" : r.status === "error" ? "error" : "pending"}
                                size="sm"
                                className="shrink-0 mt-0.5"
                              >
                                {r.status}
                              </StatusBadge>
                              <div className="flex-1 min-w-0">
                                <div className="flex items-center gap-2 text-xs text-muted-foreground">
                                  <span>{new Date(r.started_at).toLocaleString()}</span>
                                  {duration !== null && <span className="font-mono">{duration}s</span>}
                                </div>
                                {r.error && (
                                  <p className="text-xs text-destructive mt-1 font-mono line-clamp-2">{r.error}</p>
                                )}
                                {r.response_preview && (
                                  <p className="text-xs text-foreground/80 mt-1 line-clamp-2">{r.response_preview}</p>
                                )}
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </div>
                )}
              </Card>
            ))}
          </div>
        )}

      <Dialog open={formOpen} onOpenChange={(open) => { setFormOpen(open); if (!open) { setToolPolicyAllow(""); setToolPolicyDeny(""); } }}>
        <DialogContent size="xl">
          <DialogHeader className="border-b border-border/50 pb-4">
            <DialogTitle className="text-base font-bold text-foreground">
              {editId ? t("cron.editing_task") : t("cron.new_task_dialog")}
            </DialogTitle>
          </DialogHeader>
          <div className="space-y-5">
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <Field label={t("cron.field_name")}>
                <Input placeholder="daily_report" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} className="font-mono text-sm h-11" />
              </Field>
              <Field label={t("cron.field_agent")}>
                <Select value={form.agent} onValueChange={(v) => setForm({ ...form, agent: v })} disabled={!!editId}>
                  <SelectTrigger className="font-mono text-sm h-11 w-full">
                    <SelectValue placeholder={t("cron.select_agent")} />
                  </SelectTrigger>
                  <SelectContent>
                    {agents.map((a) => (
                      <SelectItem key={a} value={a} className="font-mono">{a}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </Field>
            </div>
            {/* Run once toggle */}
            <div className="flex items-center justify-between pt-2">
              <div className="space-y-0.5">
                <label className="text-sm font-medium text-muted-foreground">{t("tasks.run_once")}</label>
                <p className="text-2xs text-muted-foreground-subtle">{t("tasks.run_once_description")}</p>
              </div>
              <Switch
                checked={form.run_once ?? false}
                onCheckedChange={(v) => setForm({ ...form, run_once: v })}
              />
            </div>
            {form.run_once ? (
              <Field label={t("tasks.run_datetime")}>
                <Input
                  type="datetime-local"
                  value={form.run_at_local ?? ""}
                  onChange={(e) => setForm({
                    ...form,
                    run_at_local: e.target.value,
                    run_at: e.target.value ? new Date(e.target.value).toISOString() : null,
                  })}
                  className="font-mono text-sm h-11"
                />
              </Field>
            ) : (
              <Field label={t("cron.field_schedule")}>
                <CronSchedulePicker
                  value={form.cron}
                  onChange={(v) => setForm({ ...form, cron: v })}
                  timezone={form.timezone}
                  onTimezoneChange={(v) => setForm({ ...form, timezone: v })}
                  showDescription={false}
                />
              </Field>
            )}
            <Field label={t("tasks.start_jitter")} hint={t("tasks.jitter_hint")}>
              <Input
                type="number"
                min={0}
                max={3600}
                placeholder={t("tasks.no_jitter_hint")}
                value={form.jitter_secs ?? 0}
                onChange={(e) => setForm({ ...form, jitter_secs: parseInt(e.target.value) || 0 })}
                className="font-mono text-sm h-11"
              />
            </Field>
            <Field label={t("cron.field_task")}>
              <Textarea
                placeholder={t("cron.task_placeholder")}
                value={form.task}
                onChange={(e) => setForm({ ...form, task: e.target.value })}
                className="font-mono text-sm min-h-32 resize-y"
              />
            </Field>

            {/* Silent mode toggle */}
            <div className="flex items-center justify-between pt-2">
              <div className="space-y-0.5">
                <label className="text-sm font-medium text-muted-foreground">{t("cron.field_silent")}</label>
                <p className="text-2xs text-muted-foreground-subtle">{t("cron.silent_hint")}</p>
              </div>
              <Switch
                checked={form.silent}
                onCheckedChange={(v) => setForm({ ...form, silent: v })}
              />
            </div>

            {/* Announce channel selector (only when not silent) */}
            {!form.silent && agentChannels.length > 0 && (
              <div className="space-y-2 pt-2 border-t border-border/30">
                <label className="text-sm font-medium text-muted-foreground">{t("cron.field_announce_channel")}</label>
                <Select
                  value={form.announce_to?.channel || ""}
                  onValueChange={(chType) => {
                    const ch = agentChannels.find((c) => c.channel_type === chType);
                    if (ch) {
                      setForm({ ...form, announce_to: { channel: ch.channel_type, chat_id: form.announce_to?.chat_id || 0 } });
                    }
                  }}
                >
                  <SelectTrigger className="h-9 text-sm">
                    <SelectValue placeholder={t("cron.select_channel")} />
                  </SelectTrigger>
                  <SelectContent>
                    {agentChannels.map((ch) => (
                      <SelectItem key={ch.id} value={ch.channel_type}>
                        <span className="flex items-center gap-2">
                          <StatusDot status={ch.status === "running" ? "success" : "muted"} className="h-1.5 w-1.5" />
                          {ch.display_name}
                          <span className="text-3xs text-muted-foreground/50 font-mono">{ch.channel_type}</span>
                        </span>
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                {form.announce_to?.channel && (
                  <Field label={t("tasks.chat_id")} labelClassName="text-xs">
                    <Input
                      type="number"
                      value={form.announce_to?.chat_id || ""}
                      onChange={(e) => setForm({ ...form, announce_to: { ...form.announce_to!, chat_id: Number(e.target.value) } })}
                      placeholder="123456789"
                      className="font-mono text-sm h-9"
                    />
                  </Field>
                )}
              </div>
            )}

            {/* Tool Policy - collapsible */}
            <Collapsible className="group border-t border-border/30 pt-3">
              <CollapsibleTrigger className="cursor-pointer text-sm font-medium text-muted-foreground py-1 select-none flex items-center gap-1 w-full">
                <span>{t("cron.tool_policy")}</span>
                <span className="text-xs opacity-60 ml-1">({t("common.optional")})</span>
              </CollapsibleTrigger>
              <CollapsibleContent className="mt-2 space-y-3">
                <Field label={t("cron.tool_allow")} hint={t("cron.tool_policy_hint")} labelClassName="text-xs">
                  <Textarea
                    placeholder={"memory_search\nsearch_web"}
                    value={toolPolicyAllow}
                    onChange={(e) => setToolPolicyAllow(e.target.value)}
                    rows={3}
                    className="font-mono text-xs"
                  />
                </Field>
                <Field label={t("cron.tool_deny")} labelClassName="text-xs">
                  <Textarea
                    placeholder={"workspace_write\ncode_exec"}
                    value={toolPolicyDeny}
                    onChange={(e) => setToolPolicyDeny(e.target.value)}
                    rows={3}
                    className="font-mono text-xs"
                  />
                </Field>
              </CollapsibleContent>
            </Collapsible>
          </div>
          <DialogFooter className="border-t border-border/50 pt-4 gap-3">
            <Button variant="ghost" onClick={() => setFormOpen(false)}>{t("common.cancel")}</Button>
            <Button onClick={saveJob} disabled={mutating || !form.name.trim() || (!form.run_once && (!form.cron.trim() || !isValidCron(form.cron))) || (form.run_once && !form.run_at)}>
              {editId ? t("common.save") : t("common.create")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={doDelete}
        title={t("cron.delete_task_title")}
        description={t("cron.delete_task_description")}
      />
    </PageContainer>
  );
}
