"use client";

import { useEffect, useState, useCallback, useRef } from "react";
import { apiGet, apiPost, apiPut } from "@/lib/api";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { ErrorBanner } from "@/components/ui/error-banner";
import { PageHeader } from "@/components/ui/page-header";
import { Badge } from "@/components/ui/badge";
import { StatusBadge } from "@/components/ui/status-badge";
import { Card } from "@/components/ui/card";
import { PageContainer } from "@/components/ui/page-container";
import { SectionHeader } from "@/components/ui/section-header";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Switch } from "@/components/ui/switch";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { CronSchedulePicker } from "@/components/ui/cron-schedule-picker";
import { Field } from "@/components/ui/field";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select";
import { useAgents } from "@/lib/queries";
import type { LucideIcon } from "lucide-react";
import { Settings, Gauge, Box, GitBranch, Keyboard, RotateCcw, Save, Timer, Wrench, Bell } from "lucide-react";
import { CircularLoader } from "@/components/ui/loader";
import { toast } from "sonner";

interface ConfigData {
  [key: string]: unknown;
}

interface SubagentsSection {
  enabled: boolean;
}

/**
 * Walk a JSON Schema object and return the description at the given property path.
 * Returns undefined if the path does not exist or has no description.
 *
 * @param schema - Root schema object from GET /api/config/schema
 * @param path - Dot-separated field path as an array, e.g. ["toolgate_url"] or ["limits", "max_requests_per_minute"]
 */
function getFieldDescription(
  schema: Record<string, unknown> | null,
  path: string[]
): string | undefined {
  if (!schema) return undefined;
  let node: unknown = schema;
  for (const key of path) {
    if (typeof node !== "object" || node === null) return undefined;
    const obj = node as Record<string, unknown>;
    // Walk into .properties[key]
    const props = obj["properties"];
    if (typeof props !== "object" || props === null) return undefined;
    node = (props as Record<string, unknown>)[key];
  }
  if (typeof node !== "object" || node === null) return undefined;
  const desc = (node as Record<string, unknown>)["description"];
  return typeof desc === "string" ? desc : undefined;
}

const ALL_ALERT_EVENTS = ["down", "restart", "recovery", "resource"] as const;
const ALERT_EVENT_LABELS: Record<string, string> = {
  down: "config.alert_service_down", restart: "config.alert_restart", recovery: "config.alert_recovery", resource: "config.alert_resource",
};

export default function ConfigPage() {
  const { t } = useTranslation();
  const { data: agentList } = useAgents();
  const agents = agentList ?? [];
  const [config, setConfig] = useState<ConfigData | null>(null);
  const [error, setError] = useState("");

  const [restarting, setRestarting] = useState(false);
  const [restartConfirmOpen, setRestartConfirmOpen] = useState(false);
  const [subagentsToggling, setSubagentsToggling] = useState(false);
  const [editPublicUrl, setEditPublicUrl] = useState("");
  const [editMaxReqPerMin, setEditMaxReqPerMin] = useState("");
  const [editMaxToolConcurrency, setEditMaxToolConcurrency] = useState("");
  const [editEmbedDimensions, setEditEmbedDimensions] = useState("");
  // [agent_tool] — multi-agent timeouts
  const [editAgentToolWaitForIdle, setEditAgentToolWaitForIdle] = useState("");
  const [editAgentToolResult, setEditAgentToolResult] = useState("");
  const [editAgentToolSafety, setEditAgentToolSafety] = useState("");
  const [savingAgentTool, setSavingAgentTool] = useState(false);
  const [savingFields, setSavingFields] = useState(false);
  // [alerting]
  const [alertChannels, setAlertChannels] = useState<{ id: string; agent_name: string; channel_type: string; display_name: string }[]>([]);
  const [alertSettings, setAlertSettings] = useState<{ alert_channel_ids: string[]; alert_events: string[] }>({
    alert_channel_ids: [], alert_events: ["down", "restart", "recovery", "resource"],
  });
  const [alertSaving, setAlertSaving] = useState(false);
  // [curator]
  const [curatorEnabled, setCuratorEnabled] = useState(false);
  const [editCuratorCron, setEditCuratorCron] = useState("");
  const [editCuratorMinIdle, setEditCuratorMinIdle] = useState("");
  const [editCuratorStale, setEditCuratorStale] = useState("");
  const [editCuratorArchive, setEditCuratorArchive] = useState("");
  const [editCuratorMaxRepairs, setEditCuratorMaxRepairs] = useState("");
  const [editCuratorAgent, setEditCuratorAgent] = useState("");
  const [savingCurator, setSavingCurator] = useState(false);
  const [schema, setSchema] = useState<Record<string, unknown> | null>(null);

  const restartPollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const restartTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mountedRef = useRef(true);

  const loadConfig = useCallback(() => {
    Promise.all([
      apiGet<ConfigData>("/api/config"),
      apiGet<{ channels: { id: string; agent_name: string; channel_type: string; display_name: string }[] }>("/api/channels").catch(() => ({ channels: [] })),
      apiGet<{ alert_channel_ids: string[]; alert_events: string[] }>("/api/watchdog/settings").catch(() => ({ alert_channel_ids: [] as string[], alert_events: ["down", "restart", "recovery", "resource"] })),
    ]).then(([d, chRes, als]) => {
        setAlertChannels(chRes.channels);
        setAlertSettings(als);
        setError("");
        setEditPublicUrl((d.public_url as string) || "");
        const limits = d.limits as Record<string, unknown> | undefined;
        setEditMaxReqPerMin(String(limits?.max_requests_per_minute ?? ""));
        setEditMaxToolConcurrency(String(limits?.max_tool_concurrency ?? ""));
        const memory = d.memory as Record<string, unknown> | undefined;
        setEditEmbedDimensions(String(memory?.embed_dimensions ?? ""));
        const agentTool = d.agent_tool as Record<string, unknown> | undefined;
        setEditAgentToolWaitForIdle(String(agentTool?.message_wait_for_idle_secs ?? ""));
        setEditAgentToolResult(String(agentTool?.message_result_secs ?? ""));
        setEditAgentToolSafety(String(agentTool?.safety_timeout_secs ?? ""));
        const curator = d.curator as Record<string, unknown> | undefined;
        setCuratorEnabled(Boolean(curator?.enabled ?? false));
        setEditCuratorCron(String(curator?.cron ?? ""));
        setEditCuratorMinIdle(String(curator?.min_idle_minutes ?? ""));
        setEditCuratorStale(String(curator?.stale_after_days ?? ""));
        setEditCuratorArchive(String(curator?.archive_after_days ?? ""));
        setEditCuratorMaxRepairs(String(curator?.max_repairs_per_run ?? ""));
        setEditCuratorAgent(String(curator?.agent_name ?? ""));
        setConfig(d);
    }).catch((e) => setError(`${e}`));
  }, []);

  useEffect(() => { loadConfig(); }, [loadConfig]);

  useEffect(() => {
    apiGet<Record<string, unknown>>("/api/config/schema")
      .then(setSchema)
      .catch(() => {
        // Schema hints are non-critical — degrade gracefully if endpoint unavailable
      });
  }, []); // empty dep array: fetch once on mount

  const subagents = config?.subagents as SubagentsSection | undefined;
  const toggleSubagents = async (enabled: boolean) => {
    setSubagentsToggling(true);
    try {
      await apiPut("/api/config", { subagents_enabled: enabled });
      toast.success(enabled ? t("config.subagents_on") : t("config.subagents_off"));
      loadConfig();
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setSubagentsToggling(false);
    }
  };

  const saveAgentToolFields = async () => {
    setSavingAgentTool(true);
    try {
      const payload: Record<string, unknown> = {};
      if (editAgentToolWaitForIdle.trim()) {
        payload.agent_tool_message_wait_for_idle_secs = Number(editAgentToolWaitForIdle);
      }
      if (editAgentToolResult.trim()) {
        payload.agent_tool_message_result_secs = Number(editAgentToolResult);
      }
      if (editAgentToolSafety.trim()) {
        payload.agent_tool_safety_timeout_secs = Number(editAgentToolSafety);
      }
      await apiPut("/api/config", payload);
      toast.success(t("config.saved"));
      loadConfig();
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setSavingAgentTool(false);
    }
  };

  const saveEditableFields = async () => {
    setSavingFields(true);
    try {
      const payload: Record<string, unknown> = {
        public_url: editPublicUrl.trim(),
      };
      if (editMaxReqPerMin.trim()) payload.max_requests_per_minute = Number(editMaxReqPerMin);
      if (editMaxToolConcurrency.trim()) payload.max_tool_concurrency = Number(editMaxToolConcurrency);
      if (editEmbedDimensions.trim()) payload.embed_dimensions = Number(editEmbedDimensions);
      await apiPut("/api/config", payload);
      toast.success(t("config.saved"));
      loadConfig();
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setSavingFields(false);
    }
  };

  const saveAlertSettings = async () => {
    setAlertSaving(true);
    try {
      await apiPut("/api/watchdog/settings", alertSettings);
      toast.success(t("config.saved"));
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setAlertSaving(false);
    }
  };

  const toggleAlertChannel = (id: string) =>
    setAlertSettings((prev) => ({
      ...prev,
      alert_channel_ids: prev.alert_channel_ids.includes(id)
        ? prev.alert_channel_ids.filter((c) => c !== id)
        : [...prev.alert_channel_ids, id],
    }));

  const toggleAlertEvent = (event: string) =>
    setAlertSettings((prev) => ({
      ...prev,
      alert_events: prev.alert_events.includes(event)
        ? prev.alert_events.filter((e) => e !== event)
        : [...prev.alert_events, event],
    }));

  const saveCuratorFields = async () => {
    setSavingCurator(true);
    try {
      await apiPut("/api/curator/config", {
        enabled:             curatorEnabled,
        cron:                editCuratorCron.trim() || undefined,
        min_idle_minutes:    editCuratorMinIdle ? Number(editCuratorMinIdle) : undefined,
        stale_after_days:    editCuratorStale ? Number(editCuratorStale) : undefined,
        archive_after_days:  editCuratorArchive ? Number(editCuratorArchive) : undefined,
        max_repairs_per_run: editCuratorMaxRepairs ? Number(editCuratorMaxRepairs) : undefined,
        agent_name:          editCuratorAgent.trim() || undefined,
      });
      toast.success(t("config.saved"));
      loadConfig();
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setSavingCurator(false);
    }
  };

  // Cleanup restart polling on unmount
  useEffect(() => () => {
    mountedRef.current = false;
    if (restartPollRef.current) clearInterval(restartPollRef.current);
    if (restartTimeoutRef.current) clearTimeout(restartTimeoutRef.current);
  }, []);

  const restartCore = async () => {
    setRestarting(true);
    try {
      await apiPost("/api/restart", undefined, { "X-Confirm-Restart": "true" });
      toast.success(t("config.core_restarting"));
      // Poll until core comes back
      const poll = setInterval(async () => {
        if (!mountedRef.current) return;
        try {
          await apiGet("/health");
          clearInterval(poll);
          restartPollRef.current = null;
          if (restartTimeoutRef.current) { clearTimeout(restartTimeoutRef.current); restartTimeoutRef.current = null; }
          if (!mountedRef.current) return;
          setRestarting(false);
          loadConfig();
          toast.success(t("config.core_restarted"));
        } catch { /* still restarting */ }
      }, 2000);
      restartPollRef.current = poll;
      // Timeout after 60s
      restartTimeoutRef.current = setTimeout(() => { clearInterval(poll); restartPollRef.current = null; setRestarting(false); }, 60000);
    } catch (e) {
      toast.error(`${e}`);
      setRestarting(false);
    }
  };

  return (
    <PageContainer>
        <PageHeader
          title={t("config.title")}
          description={t("config.subtitle")}
          actions={
            <Button
              variant="destructive"
              onClick={() => setRestartConfirmOpen(true)}
              disabled={restarting}
              className="w-full md:w-auto shrink-0"
            >
              {restarting ? <CircularLoader size="sm" className="h-4 w-4" /> : <RotateCcw className="h-4 w-4" />}
              {t("config.restart_core")}
            </Button>
          }
        />

        <ConfirmDialog
          open={restartConfirmOpen}
          onClose={() => setRestartConfirmOpen(false)}
          onConfirm={() => { setRestartConfirmOpen(false); restartCore(); }}
          variant="destructive"
          title={t("config.restart_confirm_title")}
          description={t("config.restart_confirm_description")}
          confirmLabel={t("config.restart_confirm_action")}
        />

        {error && <ErrorBanner error={error} />}

        {!config && !error && (
          <div className="space-y-6">
            {[1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-40 rounded-xl" />
            ))}
          </div>
        )}

        {config && (() => {
          const serviceKeys = new Set(["toolgate_url", "public_url", "tts_proxy_url", "tools", "mcp", "tools_count", "mcp_count", "memory", "subagents", "database", "gateway", "typing", "agent_tool", "curator", "backup"]);
          const sections: Record<string, Record<string, unknown>> = {};
          const topLevel: Record<string, unknown> = {};
          for (const [key, val] of Object.entries(config)) {
            if (serviceKeys.has(key)) continue;
            if (val !== null && typeof val === "object" && !Array.isArray(val)) {
              sections[key] = val as Record<string, unknown>;
            } else {
              topLevel[key] = val;
            }
          }
          return (
            <div className="space-y-6">
              <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4 gap-4">
                {subagents && (
                  <Card className="p-4 md:p-5">
                    <SectionHeader
                      icon={GitBranch}
                      title={t("config.subagents")}
                      actions={
                        <>
                          <StatusBadge status={subagents.enabled ? "enabled" : "disabled"}>
                            {subagents.enabled ? t("config.subagents_enabled") : t("config.subagents_disabled")}
                          </StatusBadge>
                          <Switch
                            checked={subagents.enabled}
                            onCheckedChange={(v) => toggleSubagents(v)}
                            disabled={subagentsToggling}
                          />
                        </>
                      }
                    />
                    <div className="space-y-1.5">
                      {Object.entries(subagents).filter(([k]) => k !== "enabled").map(([key, val]) => (
                        <div
                          key={key}
                          className="flex flex-wrap items-center justify-between gap-2 border-b border-border/30 py-1.5 last:border-0"
                        >
                          <span className="font-mono text-xs text-muted-foreground truncate min-w-0">{key}</span>
                          <div className="shrink-0">
                            {renderValue(val, t)}
                          </div>
                        </div>
                      ))}
                    </div>
                  </Card>
                )}
                <Card className="p-4 md:p-5">
                    <SectionHeader icon={Gauge} title={t("config.editable_fields")} />
                    <div className="space-y-4">
                      <Field
                        label="public_url"
                        labelClassName="font-mono text-xs"
                        hint={getFieldDescription(schema, ["gateway", "public_url"])}
                      >
                        <Input
                          value={editPublicUrl}
                          onChange={(e) => setEditPublicUrl(e.target.value)}
                          placeholder="https://example.com"
                          className="font-mono text-sm h-9"
                        />
                      </Field>
                      <Field
                        label="max_requests_per_minute"
                        labelClassName="font-mono text-xs"
                        hint={getFieldDescription(schema, ["limits", "max_requests_per_minute"])}
                      >
                        <Input
                          type="number"
                          min={1}
                          value={editMaxReqPerMin}
                          onChange={(e) => setEditMaxReqPerMin(e.target.value)}
                          placeholder="60"
                          className="font-mono text-sm h-9"
                        />
                      </Field>
                      <Field
                        label="max_tool_concurrency"
                        labelClassName="font-mono text-xs"
                        hint={getFieldDescription(schema, ["limits", "max_tool_concurrency"])}
                      >
                        <Input
                          type="number"
                          min={1}
                          value={editMaxToolConcurrency}
                          onChange={(e) => setEditMaxToolConcurrency(e.target.value)}
                          placeholder="4"
                          className="font-mono text-sm h-9"
                        />
                      </Field>
                      <Field
                        label="embed_dimensions"
                        labelClassName="font-mono text-xs"
                        hint={
                          <>
                            <span className="block text-xs text-muted-foreground-subtle">
                              {t("config.embed_dimensions_description")}
                            </span>
                            <span className="block text-2xs text-muted-foreground-subtle leading-relaxed">
                              {t("config.embed_dimensions_hint")}
                            </span>
                          </>
                        }
                      >
                        <Input
                          type="number"
                          value={editEmbedDimensions}
                          onChange={(e) => setEditEmbedDimensions(e.target.value)}
                          placeholder={t("config.embed_dimensions_placeholder")}
                          className="font-mono text-sm h-9"
                          min={0}
                          max={8192}
                        />
                      </Field>
                      <Button
                        size="sm"
                        onClick={saveEditableFields}
                        disabled={savingFields}
                        className="gap-1.5"
                      >
                        {savingFields ? <CircularLoader size="sm" className="h-3.5 w-3.5" /> : <Save className="h-3.5 w-3.5" />}
                        {t("common.save")}
                      </Button>
                    </div>
                  </Card>
                <Card className="p-4 md:p-5">
                  <SectionHeader icon={Timer} title={t("config.agent_tool.title")} />
                  <div className="space-y-4">
                    <p className="text-xs text-muted-foreground-subtle leading-relaxed">
                      {t("config.agent_tool.description")}
                    </p>
                    <Field
                      label="message_wait_for_idle_secs"
                      labelClassName="font-mono text-xs"
                      hint={
                        <>
                          <span className="block">{t("config.agent_tool.message_wait_for_idle")}</span>
                          {(() => { const d = getFieldDescription(schema, ["agent_tool", "message_wait_for_idle_secs"]); return d ? <span className="block text-muted-foreground-subtle">{d}</span> : null; })()}
                        </>
                      }
                    >
                      <Input
                        type="number"
                        min={1}
                        max={3600}
                        value={editAgentToolWaitForIdle}
                        onChange={(e) => setEditAgentToolWaitForIdle(e.target.value)}
                        placeholder="60"
                        className="font-mono text-sm h-9"
                      />
                    </Field>
                    <Field
                      label="message_result_secs"
                      labelClassName="font-mono text-xs"
                      hint={
                        <>
                          <span className="block">{t("config.agent_tool.message_result")}</span>
                          {(() => { const d = getFieldDescription(schema, ["agent_tool", "message_result_secs"]); return d ? <span className="block text-muted-foreground-subtle">{d}</span> : null; })()}
                        </>
                      }
                    >
                      <Input
                        type="number"
                        min={1}
                        max={3600}
                        value={editAgentToolResult}
                        onChange={(e) => setEditAgentToolResult(e.target.value)}
                        placeholder="300"
                        className="font-mono text-sm h-9"
                      />
                    </Field>
                    <Field
                      label="safety_timeout_secs"
                      labelClassName="font-mono text-xs"
                      hint={
                        <>
                          <span className="block">{t("config.agent_tool.safety_timeout")}</span>
                          {(() => { const d = getFieldDescription(schema, ["agent_tool", "safety_timeout_secs"]); return d ? <span className="block text-muted-foreground-subtle">{d}</span> : null; })()}
                        </>
                      }
                    >
                      <Input
                        type="number"
                        min={1}
                        max={3600}
                        value={editAgentToolSafety}
                        onChange={(e) => setEditAgentToolSafety(e.target.value)}
                        placeholder="600"
                        className="font-mono text-sm h-9"
                      />
                    </Field>
                    <Button
                      size="sm"
                      onClick={saveAgentToolFields}
                      disabled={savingAgentTool}
                      className="gap-1.5"
                    >
                      {savingAgentTool ? <CircularLoader size="sm" className="h-3.5 w-3.5" /> : <Save className="h-3.5 w-3.5" />}
                      {t("common.save")}
                    </Button>
                  </div>
                </Card>
                <Card className="p-4 md:p-5">
                  <SectionHeader
                    icon={Wrench}
                    title={t("config.section_curator")}
                    actions={
                      <>
                        <StatusBadge status={curatorEnabled ? "enabled" : "disabled"}>
                          {curatorEnabled ? t("common.enabled") : t("common.disabled")}
                        </StatusBadge>
                        <Switch checked={curatorEnabled} onCheckedChange={setCuratorEnabled} />
                      </>
                    }
                  />
                  <div className="space-y-3">
                    <Field label="cron" labelClassName="font-mono text-xs">
                      <CronSchedulePicker
                        value={editCuratorCron}
                        onChange={setEditCuratorCron}
                        showTimezone={false}
                      />
                    </Field>
                    {[
                      { label: "min_idle_minutes", value: editCuratorMinIdle, set: setEditCuratorMinIdle, placeholder: "30" },
                      { label: "stale_after_days", value: editCuratorStale, set: setEditCuratorStale, placeholder: "30" },
                      { label: "archive_after_days", value: editCuratorArchive, set: setEditCuratorArchive, placeholder: "90" },
                      { label: "max_repairs_per_run", value: editCuratorMaxRepairs, set: setEditCuratorMaxRepairs, placeholder: "10" },
                    ].map(({ label, value, set, placeholder }) => (
                      <Field key={label} label={label} labelClassName="font-mono text-xs">
                        <Input type="number" value={value} onChange={(e) => set(e.target.value)}
                          placeholder={placeholder} className="font-mono text-sm h-9" min={0} />
                      </Field>
                    ))}
                    <Field label="agent_name" labelClassName="font-mono text-xs">
                      <Select value={editCuratorAgent} onValueChange={setEditCuratorAgent}>
                        <SelectTrigger className="h-9 font-mono text-sm">
                          <SelectValue placeholder={t("config.select_agent")} />
                        </SelectTrigger>
                        <SelectContent>
                          {agents.map((a) => (
                            <SelectItem key={a.name} value={a.name} className="font-mono text-sm">{a.name}</SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </Field>
                    <Button size="sm" onClick={saveCuratorFields} disabled={savingCurator} className="gap-1.5">
                      {savingCurator ? <CircularLoader size="sm" className="h-3.5 w-3.5" /> : <Save className="h-3.5 w-3.5" />}
                      {t("common.save")}
                    </Button>
                  </div>
                </Card>

                <Card className="p-4 md:p-5">
                  <SectionHeader icon={Bell} title={t("config.section_alerting")} />
                  <div className="space-y-4">
                    <div className="space-y-2">
                      <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">{t("config.section_channels")}</p>
                      {alertChannels.length === 0 ? (
                        <p className="text-xs text-muted-foreground-subtle italic">{t("config.no_channels")}</p>
                      ) : (
                        <div className="flex flex-col gap-1.5">
                          {alertChannels.map((ch) => {
                            const selected = alertSettings.alert_channel_ids.includes(ch.id);
                            return (
                              <Button key={ch.id} variant={selected ? "default" : "outline"} size="sm"
                                onClick={() => toggleAlertChannel(ch.id)}
                                className="w-full justify-start text-xs h-auto py-2 min-w-0">
                                <span className="truncate min-w-0">
                                  <span className="font-medium">{ch.agent_name}</span>
                                  <span className="text-muted-foreground"> / {ch.channel_type}</span>
                                  {ch.display_name !== ch.channel_type && (
                                    <span className="text-muted-foreground-subtle"> ({ch.display_name})</span>
                                  )}
                                </span>
                              </Button>
                            );
                          })}
                        </div>
                      )}
                    </div>
                    <div className="space-y-2">
                      <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">{t("config.section_events")}</p>
                      <div className="flex flex-col gap-1.5">
                        {ALL_ALERT_EVENTS.map((event) => {
                          const selected = alertSettings.alert_events.includes(event);
                          return (
                            <Button key={event} variant={selected ? "default" : "outline"} size="sm"
                              onClick={() => toggleAlertEvent(event)}
                              className="w-full justify-start text-xs">
                              {t(ALERT_EVENT_LABELS[event] as TranslationKey)}
                            </Button>
                          );
                        })}
                      </div>
                    </div>
                    <Button size="sm" onClick={saveAlertSettings} disabled={alertSaving} className="gap-1.5">
                      {alertSaving ? <CircularLoader size="sm" className="h-3.5 w-3.5" /> : <Save className="h-3.5 w-3.5" />}
                      {t("common.save")}
                    </Button>
                  </div>
                </Card>

                {Object.keys(topLevel).length > 0 && (
                  <Card className="p-4 md:p-5">
                    <SectionHeader icon={Settings} title={t("config.section_general")} />
                    <div className="space-y-1.5">
                      {Object.entries(topLevel).map(([key, val]) => (
                        <div
                          key={key}
                          className="flex flex-wrap items-center justify-between gap-2 border-b border-border/30 py-1.5 last:border-0"
                        >
                          <span className="font-mono text-xs text-muted-foreground truncate min-w-0">{key}</span>
                          <div className="shrink-0">
                            {renderValue(val, t)}
                          </div>
                        </div>
                      ))}
                    </div>
                  </Card>
                )}

                {Object.entries(sections).map(([section, values]) => {
                  const sectionIcons: Record<string, LucideIcon> = {
                    limits: Gauge,
                    sandbox: Box,
                    subagents: GitBranch,
                    typing: Keyboard,
                  };
                  return (
                  <Card key={section} className="p-4 md:p-5 min-w-0 overflow-hidden">
                    <SectionHeader icon={sectionIcons[section] ?? Settings} title={section} />
                    <div className="space-y-1.5">
                      {Object.entries(values).map(([key, val]) => (
                        <div
                          key={key}
                          className="flex flex-wrap items-start justify-between gap-2 border-b border-border/30 py-1.5 last:border-0"
                        >
                          <span className="font-mono text-xs text-muted-foreground pt-0.5 truncate min-w-0">{key}</span>
                          <div className="flex shrink-0 max-w-full sm:max-w-xs justify-end overflow-x-auto scrollbar-none">
                            {renderValue(val, t)}
                          </div>
                        </div>
                      ))}
                    </div>
                  </Card>
                  ); })}
              </div>
            </div>
          );
        })()}
    </PageContainer>
  );
}

function renderValue(val: unknown, t: (key: TranslationKey, values?: Record<string, string | number>) => string): React.ReactNode {
  if (val === null || val === undefined) {
    return <span className="text-sm text-muted-foreground-subtle italic">{t("config.value_null")}</span>;
  }
  if (typeof val === "boolean") {
    return (
      <StatusBadge status={val ? "enabled" : "disabled"}>
        {val ? t("config.value_on") : t("config.value_off")}
      </StatusBadge>
    );
  }
  if (typeof val === "number") {
    return <span className="font-mono text-sm font-bold text-primary tabular-nums shrink-0 whitespace-nowrap">{val}</span>;
  }
  if (Array.isArray(val)) {
    const hasObjects = val.some((v) => v !== null && typeof v === "object");
    if (hasObjects) {
      return (
        <pre className="max-w-full overflow-x-auto whitespace-pre-wrap font-mono text-sm text-foreground/80 bg-muted/30 p-3 rounded-lg border border-border">
          {JSON.stringify(val, null, 2)}
        </pre>
      );
    }
    return (
      <div className="flex flex-wrap sm:justify-end gap-1.5 max-w-full">
        {val.map((v, i) => (
          <Badge key={i} variant="outline-primary" className="font-mono text-xs whitespace-normal break-all leading-relaxed">
            {String(v)}
          </Badge>
        ))}
      </div>
    );
  }
  if (val && typeof val === "object") {
    return (
      <pre className="max-w-full overflow-x-auto whitespace-pre-wrap font-mono text-sm text-foreground/80 bg-muted/30 p-3 rounded-lg border border-border">
        {JSON.stringify(val, null, 2)}
      </pre>
    );
  }
  return (
    <span className="max-w-full break-all font-mono text-sm text-foreground/80">{String(val)}</span>
  );
}
