"use client";

import { useEffect, useState, useCallback, useRef } from "react";
import { apiGet, apiPost, apiPut, apiDelete } from "@/lib/api";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { ErrorBanner } from "@/components/ui/error-banner";
import { useAuthStore } from "@/stores/auth-store";
import { Button } from "@/components/ui/button";
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
import type { AgentInfo, AgentDetail, ChannelRow, SecretInfo, Provider } from "@/types/api";
import { Settings, LogOut } from "lucide-react";
import { Loader } from "@/components/ui/loader";
import { PROVIDERS, FALLBACK_MODELS } from "./RoutingRulesEditor";
import {
  AgentEditDialog,
  ChannelDialog,
  DeleteChannelDialog,
  type FormState,
} from "./AgentEditDialog";
import { describeCron } from "@/lib/cron";

const LANGUAGES: { value: string; labelKey?: TranslationKey; label?: string }[] = [
  { value: "ru", labelKey: "agents.lang_ru" },
  { value: "en", labelKey: "agents.lang_en" },
  { value: "es", label: "Espa\u00f1ol" },
  { value: "de", label: "Deutsch" },
  { value: "fr", label: "Fran\u00e7ais" },
  { value: "zh", label: "\u4e2d\u6587" },
  { value: "ja", label: "\u65e5\u672c\u8a9e" },
  { value: "ko", label: "\ud55c\uad6d\uc5b4" },
  { value: "pt", label: "Portugu\u00eas" },
  { value: "it", label: "Italiano" },
  { value: "ar", label: "\u0627\u0644\u0639\u0631\u0628\u064a\u0629" },
  { value: "hi", label: "\u0939\u093f\u0928\u094d\u0926\u0940" },
];

export const emptyForm: FormState = {
  name: "",
  language: "ru",
  provider: "minimax",
  model: "MiniMax-M2.5",
  providerConnection: "",
  temperature: "1.0",
  maxTokens: "",
  hbEnabled: false,
  hbCron: "",
  hbTimezone: "Europe/Samara",
  toolsEnabled: false,
  toolsAllowAll: true,
  toolsDenyAllOthers: false,
  toolsAllow: "",
  toolsDeny: "",
  compEnabled: false,
  compThreshold: "0.8",
  compPreserveToolCalls: false,
  compPreserveLastN: "10",
  maxToolsInContext: "",
  tlEnabled: false,
  tlMaxIterations: "50",
  tlCompactOnOverflow: true,
  tlDetectLoops: true,
  tlWarnThreshold: "5",
  tlBreakThreshold: "10",
  tlMaxAutoContinues: "5",
  sessionEnabled: false,
  sessionDmScope: "per-channel-peer",
  sessionTtlDays: "30",
  sessionMaxMessages: "0",
  routing: [],
  voice: "",
  icon: "",
  approvalEnabled: false,
  approvalRequireFor: [],
  approvalCategories: [],
  approvalTimeout: "300",
  compMaxContextTokens: "",
  sessionPruneToolOutput: "",
  maxHistoryMessages: "",
  hbAnnounceTo: "",
  toolGroupGit: true,
  toolGroupManagement: true,
  toolGroupSkillEditing: true,
  toolGroupSessionTools: true,
  hooksLogAll: false,
  hooksBlockTools: "",
  dailyBudgetTokens: "0",
  accessEnabled: false,
  accessMode: "open",
  accessOwnerId: "",
  fallbackProvider: "",
};

export function detailToForm(d: AgentDetail): FormState {
  return {
    name: d.name,
    language: d.language || "ru",
    provider: d.provider,
    model: d.model,
    providerConnection: d.provider_connection ?? "",
    temperature: String(d.temperature),
    maxTokens: d.max_tokens != null ? String(d.max_tokens) : "",
    hbEnabled: !!d.heartbeat,
    hbCron: d.heartbeat?.cron ?? "",
    hbTimezone: d.heartbeat?.timezone ?? "UTC",
    toolsEnabled: !!d.tools,
    toolsAllowAll: d.tools?.allow_all ?? true,
    toolsDenyAllOthers: d.tools?.deny_all_others ?? false,
    toolsAllow: d.tools?.allow.join(", ") ?? "",
    toolsDeny: d.tools?.deny.join(", ") ?? "",
    compEnabled: !!d.compaction && d.compaction.enabled,
    compThreshold: String(d.compaction?.threshold ?? 0.8),
    compPreserveToolCalls: d.compaction?.preserve_tool_calls ?? false,
    compPreserveLastN: String(d.compaction?.preserve_last_n ?? 10),
    maxToolsInContext: d.max_tools_in_context != null ? String(d.max_tools_in_context) : "",
    tlEnabled: !!d.tool_loop,
    tlMaxIterations: String(d.tool_loop?.max_iterations ?? 50),
    tlCompactOnOverflow: d.tool_loop?.compact_on_overflow ?? true,
    tlDetectLoops: d.tool_loop?.detect_loops ?? true,
    tlWarnThreshold: String(d.tool_loop?.warn_threshold ?? 5),
    tlBreakThreshold: String(d.tool_loop?.break_threshold ?? 10),
    tlMaxAutoContinues: String(d.tool_loop?.max_auto_continues ?? 5),
    sessionEnabled: !!d.session,
    sessionDmScope: d.session?.dm_scope ?? "per-channel-peer",
    sessionTtlDays: String(d.session?.ttl_days ?? 30),
    sessionMaxMessages: String(d.session?.max_messages ?? 0),
    // Map AgentDetailRoutingDto (connection field) → RoutingRule (provider field)
    // for the form editor. The write path (formToPayload) sends back `provider`.
    routing: (d.routing || []).map((r) => ({
      provider: r.connection ?? "",
      model: r.model ?? "",
      condition: r.condition,
      temperature: r.temperature ?? null,
      cooldown_secs: r.cooldown_secs,
    })),
    voice: d.voice || "",
    icon: d.icon || "",
    approvalEnabled: !!d.approval?.enabled,
    approvalRequireFor: d.approval?.require_for ?? [],
    approvalCategories: d.approval?.require_for_categories ?? [],
    approvalTimeout: String(d.approval?.timeout_seconds ?? 300),
    compMaxContextTokens: d.compaction?.max_context_tokens != null ? String(d.compaction.max_context_tokens) : "",
    sessionPruneToolOutput: d.session?.prune_tool_output_after_turns != null ? String(d.session.prune_tool_output_after_turns) : "",
    maxHistoryMessages: d.max_history_messages != null ? String(d.max_history_messages) : "",
    hbAnnounceTo: d.heartbeat?.announce_to ?? "",
    toolGroupGit: d.tools?.groups?.git ?? true,
    toolGroupManagement: d.tools?.groups?.tool_management ?? true,
    toolGroupSkillEditing: d.tools?.groups?.skill_editing ?? true,
    toolGroupSessionTools: d.tools?.groups?.session_tools ?? true,
    hooksLogAll: d.hooks?.log_all_tool_calls ?? false,
    hooksBlockTools: d.hooks?.block_tools?.join(", ") ?? "",
    dailyBudgetTokens: String(d.daily_budget_tokens ?? 0),
    accessEnabled: !!d.access,
    accessMode: d.access?.mode ?? "open",
    accessOwnerId: d.access?.owner_id ?? "",
    fallbackProvider: d.fallback_provider ?? "",
  };
}

export function formToPayload(f: FormState) {
  const splitList = (s: string) =>
    s.split(",").map((x) => x.trim()).filter(Boolean);

  return {
    name: f.name,
    language: f.language,
    provider: f.provider,
    model: f.model,
    provider_connection: f.providerConnection || null,
    temperature: Number.isFinite(parseFloat(f.temperature)) ? parseFloat(f.temperature) : 1.0,
    max_tokens: f.maxTokens ? parseInt(f.maxTokens) : null,
    access: f.accessEnabled
      ? {
          mode: f.accessMode,
          owner_id: f.accessOwnerId || undefined,
        }
      : null,
    heartbeat: f.hbEnabled
      ? { cron: f.hbCron, timezone: f.hbTimezone || null, announce_to: f.hbAnnounceTo || null }
      : null,
    tools: f.toolsEnabled
      ? {
          allow_all: f.toolsAllowAll,
          deny_all_others: f.toolsDenyAllOthers,
          allow: splitList(f.toolsAllow),
          deny: splitList(f.toolsDeny),
          groups: {
            git: f.toolGroupGit,
            tool_management: f.toolGroupManagement,
            skill_editing: f.toolGroupSkillEditing,
            session_tools: f.toolGroupSessionTools,
          },
        }
      : null,
    compaction: f.compEnabled
      ? {
          enabled: true,
          threshold: parseFloat(f.compThreshold) || 0.8,
          preserve_tool_calls: f.compPreserveToolCalls,
          preserve_last_n: parseInt(f.compPreserveLastN) || 10,
          max_context_tokens: f.compMaxContextTokens ? parseInt(f.compMaxContextTokens) : null,
        }
      : null,
    max_tools_in_context: f.maxToolsInContext.trim() !== "" ? parseInt(f.maxToolsInContext) || null : null,
    session: f.sessionEnabled
      ? {
          dm_scope: f.sessionDmScope,
          ttl_days: parseInt(f.sessionTtlDays) || 30,
          max_messages: parseInt(f.sessionMaxMessages) || 0,
          prune_tool_output_after_turns: f.sessionPruneToolOutput ? parseInt(f.sessionPruneToolOutput) : null,
        }
      : null,
    routing: f.routing.length > 0 ? f.routing.map((r) => ({
      connection: r.provider || null,
      model: r.model || null,
      condition: r.condition || "default",
      temperature: r.temperature ?? null,
      cooldown_secs: r.cooldown_secs ?? 60,
    })) : null,
    voice: f.voice || null,
    icon: f.icon || null,
    approval: f.approvalEnabled
      ? {
          enabled: true,
          require_for: f.approvalRequireFor,
          require_for_categories: f.approvalCategories,
          timeout_seconds: parseInt(f.approvalTimeout) || 300,
        }
      : null,
    tool_loop: f.tlEnabled
      ? {
          max_iterations: f.tlMaxIterations.trim() !== "" ? parseInt(f.tlMaxIterations) : 50,
          compact_on_overflow: f.tlCompactOnOverflow,
          detect_loops: f.tlDetectLoops,
          warn_threshold: parseInt(f.tlWarnThreshold) || 5,
          break_threshold: parseInt(f.tlBreakThreshold) || 10,
          max_auto_continues: parseInt(f.tlMaxAutoContinues) || 5,
        }
      : null,
    hooks: (f.hooksLogAll || f.hooksBlockTools.trim())
      ? {
          log_all_tool_calls: f.hooksLogAll,
          block_tools: splitList(f.hooksBlockTools),
        }
      : null,
    max_history_messages: f.maxHistoryMessages ? parseInt(f.maxHistoryMessages) : null,
    daily_budget_tokens: parseInt(f.dailyBudgetTokens) || 0,
    fallback_provider: f.fallbackProvider || null,
  };
}

export default function AgentsPage() {
  const { t } = useTranslation();
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [formOpen, setFormOpen] = useState(false);
  const [editName, setEditName] = useState<string | null>(null);
  const [form, setForm] = useState<FormState>(emptyForm);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

  // Channels
  const [channels, setChannels] = useState<ChannelRow[]>([]);
  const [channelSaving, setChannelSaving] = useState(false);
  const [channelDialogOpen, setChannelDialogOpen] = useState(false);
  const [channelDialogId, setChannelDialogId] = useState<string | null>(null);
  const [channelForm, setChannelForm] = useState({ channel_type: "telegram", display_name: "", bot_token: "", api_url: "" });
  const [deleteChannelId, setDeleteChannelId] = useState<string | null>(null);

  // TTS voices
  const [voices, setVoices] = useState<{ id: string; name: string; description?: string }[]>([]);
  const loadVoices = useCallback(async () => {
    try {
      const data = await apiGet<{ voices: { id: string; name: string; description?: string }[] }>("/api/tts/voices");
      setVoices(data.voices || []);
    } catch (e) {
      console.warn("[agents] failed to load voices:", e);
      setVoices([]);
    }
  }, []);

  // Tools list
  const [toolNames, setToolNames] = useState<string[]>([]);
  const toolsLoadedRef = useRef(false);
  const loadTools = useCallback(async () => {
    if (toolsLoadedRef.current) return;
    try {
      const data = await apiGet<{ tools: string[] }>("/api/tool-definitions");
      setToolNames((data.tools || []).sort());
      toolsLoadedRef.current = true;
    } catch (e) {
      console.warn("[agents] failed to load tools:", e);
      setToolNames([]);
    }
  }, []);

  // Secret names
  const [secretNames, setSecretNames] = useState<string[]>([]);
  const secretsLoadedRef = useRef(false);
  const loadSecrets = useCallback(async () => {
    if (secretsLoadedRef.current) return;
    try {
      const data = await apiGet<{ secrets: SecretInfo[] }>("/api/secrets");
      setSecretNames((data.secrets || []).map((s) => s.name).sort());
      secretsLoadedRef.current = true;
    } catch (e) {
      console.warn("[agents] failed to load secrets:", e);
      setSecretNames([]);
    }
  }, []);

  // Dynamic model discovery
  const [discoveredModels, setDiscoveredModels] = useState<Record<string, string[]>>({});
  const [modelsLoading, setModelsLoading] = useState<string | null>(null);
  const discoveredModelsRef = useRef(discoveredModels);
  discoveredModelsRef.current = discoveredModels;

  const fetchModels = useCallback(async (providerName: string, providerConnection?: string) => {
    const cacheKey = providerConnection || providerName;
    if (discoveredModelsRef.current[cacheKey]) return;
    setModelsLoading(cacheKey);
    try {
      // Resolve provider UUID: look up by connection name or provider type name
      const lookup = providerConnection || providerName;
      const providersData = await apiGet<{ providers: Provider[] }>("/api/providers");
      const match = (providersData.providers || []).find(
        (p) => p.name === lookup || p.provider_type === providerName
      );
      if (!match) {
        setDiscoveredModels((prev) => ({ ...prev, [cacheKey]: FALLBACK_MODELS[providerName] ?? [] }));
        return;
      }
      const data = await apiGet<{ models: Array<string | { id: string }> }>(`/api/providers/${match.id}/models`);
      const ids = (data.models || []).map((m) => typeof m === "string" ? m : m.id);
      setDiscoveredModels((prev) => ({ ...prev, [cacheKey]: ids.length > 0 ? ids : (FALLBACK_MODELS[providerName] ?? []) }));
    } catch {
      setDiscoveredModels((prev) => ({ ...prev, [cacheKey]: FALLBACK_MODELS[providerName] ?? [] }));
    } finally {
      setModelsLoading(null);
    }
  }, []);

  const loadChannels = useCallback(async (name: string) => {
    try {
      const data = await apiGet<ChannelRow[]>(`/api/agents/${name}/channels`);
      setChannels(Array.isArray(data) ? data : []);
    } catch (e) {
      console.warn("[agents] failed to load channels:", e);
      setChannels([]);
    }
  }, []);

  const load = useCallback(async () => {
    try {
      const data = await apiGet<{ agents: AgentInfo[] }>("/api/agents");
      setAgents(data.agents || []);
      setError("");
    } catch (e) {
      setError(`${e}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const openCreate = useCallback(() => {
    setEditName(null);
    setForm({ ...emptyForm });
    setFormOpen(true);
    loadVoices();
    loadTools();
    loadSecrets();
    fetchModels(emptyForm.provider || "minimax", emptyForm.providerConnection || undefined);
  }, [loadVoices, loadTools, loadSecrets, fetchModels]);

  const openEdit = useCallback(async (name: string) => {
    try {
      const detail = await apiGet<AgentDetail>(`/api/agents/${name}`);
      setEditName(name);
      setForm(detailToForm(detail));
      setChannels([]);
      setFormOpen(true);
      fetchModels(detail.provider || "minimax", detail.provider_connection || undefined);
      loadChannels(name);
      loadVoices();
      loadTools();
      loadSecrets();
    } catch (e) {
      setError(`${e}`);
    }
  }, [fetchModels, loadChannels, loadVoices, loadTools, loadSecrets]);

  const restartChannel = useCallback(async (channelId: string) => {
    if (!editName) return;
    setChannelSaving(true);
    try {
      await apiPost(`/api/agents/${editName}/channels/${channelId}/restart`, {});
      await loadChannels(editName);
    } catch (e) {
      setError(`${e}`);
    }
    setChannelSaving(false);
  }, [editName, loadChannels]);

  const openChannelDialog = useCallback((ch?: ChannelRow) => {
    if (ch) {
      const cfg = ch.config || {};
      setChannelDialogId(ch.id);
      setChannelForm({
        channel_type: ch.channel_type,
        display_name: ch.display_name,
        bot_token: (cfg.bot_token as string) || "",
        api_url: (cfg.api_url as string) || "",
      });
    } else {
      setChannelDialogId(null);
      setChannelForm({ channel_type: "telegram", display_name: "", bot_token: "", api_url: "" });
    }
    setChannelDialogOpen(true);
  }, []);

  const saveChannel = useCallback(async () => {
    if (!editName) return;
    setChannelSaving(true);
    try {
      if (channelDialogId) {
        await apiPut(`/api/agents/${editName}/channels/${channelDialogId}`, {
          display_name: channelForm.display_name,
          config: { bot_token: channelForm.bot_token, api_url: channelForm.api_url },
        });
      } else {
        await apiPost(`/api/agents/${editName}/channels`, {
          channel_type: channelForm.channel_type,
          display_name: channelForm.display_name,
          config: { bot_token: channelForm.bot_token, api_url: channelForm.api_url },
        });
      }
      setChannelDialogOpen(false);
      await loadChannels(editName);
    } catch (e) {
      setError(`${e}`);
    }
    setChannelSaving(false);
  }, [editName, channelDialogId, channelForm, loadChannels]);

  const deleteChannel = useCallback(async (channelId: string) => {
    if (!editName) return;
    setChannelSaving(true);
    try {
      await apiDelete(`/api/agents/${editName}/channels/${channelId}`);
      setDeleteChannelId(null);
      await loadChannels(editName);
    } catch (e) {
      setError(`${e}`);
    }
    setChannelSaving(false);
  }, [editName, loadChannels]);

  const saveAgent = useCallback(async () => {
    const temp = parseFloat(form.temperature);
    if (isNaN(temp) || temp < 0 || temp > 2) {
      setError(t("agents.temperature_error"));
      return;
    }
    setSaving(true);
    setError("");
    try {
      const payload = formToPayload(form);
      if (editName) {
        await apiPut(`/api/agents/${editName}`, payload);
      } else {
        await apiPost("/api/agents", payload);
      }
      setFormOpen(false);
      await load();
      useAuthStore.getState().restore();
    } catch (e) {
      setError(`${e}`);
    }
    setSaving(false);
  }, [form, editName, t, load]);

  const doDelete = useCallback(async () => {
    if (!deleteTarget) return;
    setSaving(true);
    try {
      await apiDelete(`/api/agents/${deleteTarget}`);
      setDeleteTarget(null);
      await load();
    } catch (e) {
      setError(`${e}`);
    }
    setSaving(false);
  }, [deleteTarget, load]);

  const isValidName = /^[a-zA-Z0-9_-]+$/.test(form.name.trim());
  const canSave =
    isValidName &&
    form.provider.trim().length > 0 &&
    form.model.trim().length > 0 &&
    (!form.hbEnabled || form.hbCron.trim().length > 0);

  const upd = (patch: Partial<FormState>) => setForm((f) => ({ ...f, ...patch }));

  return (
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
      <div className="mb-8 md:mb-10 flex flex-col md:flex-row md:items-center justify-between gap-4">
        <div className="flex flex-col gap-1">
          <h2 className="font-display text-lg font-bold tracking-tight">{t("agents.title")}</h2>
          <span className="text-sm text-muted-foreground">
            {t("agents.subtitle")}
          </span>
        </div>
        <Button
          size="lg"
          onClick={openCreate}
          className="w-full md:w-auto font-semibold"
        >
          {t("agents.new_agent")}
        </Button>
      </div>

      {error && <ErrorBanner error={error} />}

      {loading ? (
        <div className="grid grid-cols-1 gap-6 md:grid-cols-2 xl:grid-cols-3">
          {[1, 2, 3].map((i) => (
            <div key={i} className="h-64 rounded-xl border border-border bg-muted/20 animate-pulse" />
          ))}
        </div>
      ) : agents.length === 0 ? (
        <div className="flex h-64 flex-col items-center justify-center rounded-2xl border border-dashed border-border bg-muted/10">
          <p className="font-mono text-sm uppercase tracking-wider text-muted-foreground/70">{t("agents.no_active_agents")}</p>
        </div>
      ) : (
        <div className="grid grid-cols-1 gap-6 md:grid-cols-2 xl:grid-cols-3">
          {agents.map((a) => (
            <div key={a.name} className="group neu-card neu-hover p-4 md:p-5 transition-all duration-300 overflow-hidden flex flex-col">
              <div className="flex items-start gap-3 mb-4 min-w-0">
                <div className="relative shrink-0">
                  <div className="flex h-11 w-11 items-center justify-center rounded-lg border border-primary/20 bg-muted/50 shadow-inner group-hover:border-primary/50 transition-colors overflow-hidden">
                    {a.icon ? (
                      <img src={`/uploads/${a.icon}`} alt={a.name} loading="lazy" className="h-full w-full object-cover" />
                    ) : (
                      <span className="font-mono text-lg font-black text-primary/80 group-hover:text-primary transition-colors">
                        {a.name.charAt(0).toUpperCase()}
                      </span>
                    )}
                  </div>
                  <div className={`absolute -bottom-0.5 -right-0.5 h-3 w-3 rounded-full border-2 border-background ${a.is_running ? "bg-success" : "bg-muted-foreground/50"}`} />
                </div>
                <div className="flex flex-col gap-0.5 min-w-0 flex-1">
                  <div className="flex items-center gap-2 min-w-0">
                    <h3 className="font-mono text-sm font-bold tracking-tight text-foreground truncate">{a.name}</h3>
                    {a.is_running ? (
                      <div className="flex items-center gap-1 shrink-0 rounded-full border border-success/30 bg-success/10 px-2 py-0.5">
                        <div className="h-1.5 w-1.5 animate-pulse rounded-full bg-success" />
                        <span className="text-[10px] font-semibold text-success">{t("agents.active")}</span>
                      </div>
                    ) : (
                      <div className="flex items-center gap-1 shrink-0 rounded-full border border-border bg-muted/40 px-2 py-0.5">
                        <div className="h-1.5 w-1.5 rounded-full bg-muted-foreground/50" />
                        <span className="text-[10px] font-semibold text-muted-foreground/80">{t("agents.inactive")}</span>
                      </div>
                    )}
                  </div>
                  <span className="text-xs text-muted-foreground truncate">{a.model}</span>
                </div>
              </div>

              <div className="space-y-3 mb-4 flex-1">
                <InfoRow label={t("agents.provider")} value={a.provider_connection || (PROVIDERS.find((p) => p.value === a.provider)?.label ?? a.provider)} />
                {a.routing_count > 0 && (
                  <InfoRow label={t("agents.routing")} value={t("agents.routing_rules_count", { count: a.routing_count })} />
                )}
                <InfoRow label={t("agents.language")} value={(() => { const l = LANGUAGES.find((l) => l.value === a.language); return l?.labelKey ? t(l.labelKey) : l?.label ?? a.language; })()} />

                {a.has_heartbeat && (
                  <div className="flex flex-col gap-1 border-b border-border/50 py-2">
                    <span className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/80">{t("agents.schedule")}</span>
                    <span className="font-mono text-xs font-bold text-primary tabular-nums truncate">
                      {a.heartbeat_cron ? describeCron(a.heartbeat_cron, t) : "\u2014"}{" · "}{a.heartbeat_timezone || "UTC"}
                    </span>
                  </div>
                )}
              </div>

              <div className="grid grid-cols-2 gap-2 mt-auto">
                <Button
                  variant="outline"
                  onClick={() => openEdit(a.name)}
                  className={`h-8 border-primary/20 bg-primary/5 text-primary hover:bg-primary/20 font-mono text-[10px] uppercase tracking-wider ${a.base ? "col-span-2" : ""}`}
                >
                  <Settings className="h-3.5 w-3.5 mr-1" /> {t("agents.configure")}
                </Button>
                {!a.base && (
                  <Button
                    variant="outline"
                    onClick={() => setDeleteTarget(a.name)}
                    disabled={saving}
                    className="h-8 border-destructive/20 bg-destructive/5 text-destructive hover:text-destructive hover:bg-destructive/20 font-mono text-[10px] uppercase tracking-wider"
                    aria-label={t("agents.delete_agent_aria", { name: a.name })}
                  >
                    {saving ? <Loader variant="circular" size="sm" /> : <LogOut className="h-3.5 w-3.5 rotate-90 mr-1" />}
                    {!saving && t("common.delete")}
                  </Button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Create / Edit Dialog */}
      <AgentEditDialog
        open={formOpen}
        onOpenChange={setFormOpen}
        editName={editName}
        form={form}
        upd={upd}
        saving={saving}
        canSave={canSave}
        onSave={saveAgent}
        discoveredModels={discoveredModels}
        modelsLoading={modelsLoading}
        fetchModels={fetchModels}
        toolNames={toolNames}
        secretNames={secretNames}
        voices={voices}
        channels={channels}
        channelSaving={channelSaving}
        onOpenChannelDialog={openChannelDialog}
        onRestartChannel={restartChannel}
        onDeleteChannelRequest={(id) => setDeleteChannelId(id)}
      />

      {/* Channel Dialog (Create / Edit) */}
      <ChannelDialog
        open={channelDialogOpen}
        onOpenChange={setChannelDialogOpen}
        channelDialogId={channelDialogId}
        channelForm={channelForm}
        setChannelForm={setChannelForm}
        channelSaving={channelSaving}
        onSave={saveChannel}
      />

      {/* Delete Channel Confirmation */}
      <DeleteChannelDialog
        deleteChannelId={deleteChannelId}
        onOpenChange={() => setDeleteChannelId(null)}
        onConfirm={deleteChannel}
      />

      {/* Delete Agent Confirmation */}
      <AlertDialog
        open={!!deleteTarget}
        onOpenChange={(o) => {
          if (!o) setDeleteTarget(null);
        }}
      >
        <AlertDialogContent className="border-border shadow-2xl rounded-xl">
          <AlertDialogHeader>
            <AlertDialogTitle className="text-base font-bold text-destructive">{t("agents.delete_agent_title", { name: deleteTarget ?? "" })}</AlertDialogTitle>
            <AlertDialogDescription className="text-sm text-muted-foreground mt-2">
              {t("agents.delete_agent_description")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter className="mt-6">
            <AlertDialogCancel className="border-border hover:bg-muted">{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              onClick={doDelete}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {t("common.delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

function InfoRow({
  label,
  value,
  mono,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <div className="flex items-center justify-between border-b border-border/50 py-2.5">
      <span className="text-sm text-muted-foreground">{label}</span>
      <span
        className={`max-w-[60%] truncate text-right font-medium transition-colors group-hover:text-foreground ${mono ? "font-mono text-sm text-foreground/80" : "text-foreground text-sm"}`}
      >
        {value}
      </span>
    </div>
  );
}
