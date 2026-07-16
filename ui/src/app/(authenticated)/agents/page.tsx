"use client";

import { useEffect, useState, useCallback, useRef } from "react";
import { apiGet, apiPost, apiPut, apiDelete } from "@/lib/api";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { ErrorBanner } from "@/components/ui/error-banner";
import { PageHeader } from "@/components/ui/page-header";
import { SearchInput } from "@/components/ui/search-input";
import { useAuthStore } from "@/stores/auth-store";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { PageContainer } from "@/components/ui/page-container";
import { IconTile } from "@/components/ui/icon-tile";
import { StatusBadge } from "@/components/ui/status-badge";
import { StatusDot } from "@/components/ui/status-dot";
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
import { Settings, LogOut, Bot, Plus, Search } from "lucide-react";
import { Loader } from "@/components/ui/loader";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import { FALLBACK_MODELS } from "./RoutingRulesEditor";
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
  profile: "Default",
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
  srEnabled: false,
  srMinToolCalls: "3",
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
  iconUrl: "",
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
  hooksWebhooks: [],
  dailyBudgetTokens: "0",
  accessEnabled: true,
  accessMode: "restricted",
  accessOwnerId: "",
  toolDispatcherEnabled: false,
  toolDispatcherCoreExtra: [],
  toolDispatcherPromotionMax: "8",
  soulEnabled: false,
  soulReflectionThreshold: "150",
  soulCooldownMin: "60",
  soulTopK: "6",
  soulBudgetTokens: "800",
  soulMaxEvents: "10",
  driftEnabled: false,
  driftThreshold: "0.15",
  driftMinHistory: "6",
  driftBaselineTurns: "3",
  driftCorrect: false,
  driftAnchor: "",
  initiativeEnabled: false,
  initiativeProposalCap: "1",
  initiativeDecompose: false,
  initiativeDailyPlan: false,
  initiativeAutoApprove: false,
  initiativeTokenBudget: "0",
  emotionEnabled: false,
  emotionK: "3",
  emotionBlendRate: "0.3",
  emotionHalfLife: "12",
};

export function detailToForm(d: AgentDetail): FormState {
  return {
    name: d.name,
    language: d.language || "ru",
    profile: d.profile || "Default",
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
    iconUrl: d.icon_url || "",
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
    hooksWebhooks: d.hooks?.webhooks ?? [],
    dailyBudgetTokens: String(d.daily_budget_tokens ?? 0),
    accessEnabled: !!d.access,
    accessMode: d.access?.mode ?? "restricted",
    accessOwnerId: d.access?.owner_id ?? "",
    srEnabled: !!d.skill_review && d.skill_review.enabled,
    srMinToolCalls: String(d.skill_review?.min_tool_calls ?? 3),
    toolDispatcherEnabled: d.tool_dispatcher?.enabled ?? false,
    toolDispatcherCoreExtra: d.tool_dispatcher?.core_extra ?? [],
    toolDispatcherPromotionMax: d.tool_dispatcher?.promotion_max != null ? String(d.tool_dispatcher.promotion_max) : '8',
    soulEnabled: d.soul?.enabled ?? false,
    soulReflectionThreshold: String(d.soul?.reflection_threshold ?? 150),
    soulCooldownMin: String(d.soul?.reflection_cooldown_minutes ?? 60),
    soulTopK: String(d.soul?.context_top_k ?? 6),
    soulBudgetTokens: String(d.soul?.context_budget_tokens ?? 800),
    soulMaxEvents: String(d.soul?.max_events_per_session ?? 10),
    driftEnabled: d.drift?.enabled ?? false,
    driftThreshold: String(d.drift?.threshold ?? 0.15),
    driftMinHistory: String(d.drift?.min_history ?? 6),
    driftBaselineTurns: String(d.drift?.baseline_turns ?? 3),
    driftCorrect: d.drift?.correct ?? false,
    driftAnchor: d.drift?.anchor ?? "",
    initiativeEnabled: d.initiative?.enabled ?? false,
    initiativeProposalCap: String(d.initiative?.daily_proposal_cap ?? 1),
    initiativeDecompose: d.initiative?.decompose ?? false,
    initiativeDailyPlan: d.initiative?.daily_plan ?? false,
    initiativeAutoApprove: d.initiative?.auto_approve_day_plan ?? false,
    initiativeTokenBudget: String(d.initiative?.daily_token_budget ?? 0),
    emotionEnabled: d.emotion?.enabled ?? false,
    emotionK: String(d.emotion?.intensity_importance_k ?? 3),
    emotionBlendRate: String(d.emotion?.blend_rate ?? 0.3),
    emotionHalfLife: String(d.emotion?.decay_half_life_hours ?? 12),
  };
}

export function formToPayload(f: FormState) {
  const splitList = (s: string) =>
    s.split(",").map((x) => x.trim()).filter(Boolean);

  return {
    name: f.name,
    language: f.language,
    profile: f.profile || "Default",
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
    approval: f.approvalEnabled
      ? {
          enabled: true,
          require_for: f.approvalRequireFor,
          require_for_categories: f.approvalCategories,
          timeout_seconds: parseInt(f.approvalTimeout) || 300,
        }
      : null,
    skill_review: f.srEnabled
      ? { enabled: true, min_tool_calls: parseInt(f.srMinToolCalls) || 3 }
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
    hooks: (f.hooksLogAll || f.hooksBlockTools.trim() || f.hooksWebhooks.length)
      ? {
          log_all_tool_calls: f.hooksLogAll,
          block_tools: splitList(f.hooksBlockTools),
          webhooks: f.hooksWebhooks,
        }
      : null,
    max_history_messages: f.maxHistoryMessages ? parseInt(f.maxHistoryMessages) : null,
    daily_budget_tokens: parseInt(f.dailyBudgetTokens) || 0,
    tool_dispatcher: {
      enabled: f.toolDispatcherEnabled,
      core_extra: f.toolDispatcherCoreExtra,
      promotion_max: f.toolDispatcherPromotionMax.trim() !== ""
        ? Math.max(0, Math.min(16, parseInt(f.toolDispatcherPromotionMax) || 0))
        : 8,
    },
    soul: {
      enabled: f.soulEnabled,
      reflection_threshold: parseFloat(f.soulReflectionThreshold) || 150,
      // M4: preserve an explicit 0 (server accepts 0..=1440) instead of
      // falling back to the default via `|| 60`, which would clobber it.
      reflection_cooldown_minutes: f.soulCooldownMin.trim() === ""
        ? 60
        : (Number.isFinite(parseInt(f.soulCooldownMin)) ? parseInt(f.soulCooldownMin) : 60),
      context_top_k: parseInt(f.soulTopK) || 6,
      context_budget_tokens: parseInt(f.soulBudgetTokens) || 800,
      max_events_per_session: parseInt(f.soulMaxEvents) || 10,
    },
    drift: {
      enabled: f.driftEnabled,
      threshold: parseFloat(f.driftThreshold) || 0.15,
      min_history: parseInt(f.driftMinHistory) || 6,
      baseline_turns: parseInt(f.driftBaselineTurns) || 3,
      correct: f.driftCorrect,
      anchor: f.driftAnchor.trim() !== "" ? f.driftAnchor : null,
    },
    initiative: {
      enabled: f.initiativeEnabled,
      daily_proposal_cap: parseInt(f.initiativeProposalCap) || 1,
      decompose: f.initiativeDecompose,
      daily_plan: f.initiativeDailyPlan,
      auto_approve_day_plan: f.initiativeAutoApprove,
      daily_token_budget: parseInt(f.initiativeTokenBudget) || 0,
    },
    emotion: {
      enabled: f.emotionEnabled,
      intensity_importance_k: parseFloat(f.emotionK) || 3,
      blend_rate: parseFloat(f.emotionBlendRate) || 0.3,
      decay_half_life_hours: parseFloat(f.emotionHalfLife) || 12,
    },
  };
}

export default function AgentsPage() {
  const { t } = useTranslation();
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [agentSearch, setAgentSearch] = useState("");
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

  // Tools list
  const [toolNames, setToolNames] = useState<string[]>([]);
  const toolsLoadedRef = useRef(false);
  const loadTools = useCallback(async () => {
    if (toolsLoadedRef.current) return;
    try {
      const data = await apiGet<{ tools: string[] }>("/api/tool-definitions");
      setToolNames((data.tools || []).sort());
      toolsLoadedRef.current = true;
    } catch {
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
    } catch {
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
    } catch {
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
    loadTools();
    loadSecrets();
  }, [loadTools, loadSecrets]);

  const openEdit = useCallback(async (name: string) => {
    try {
      const detail = await apiGet<AgentDetail>(`/api/agents/${name}`);
      setEditName(name);
      setForm(detailToForm(detail));
      setChannels([]);
      setFormOpen(true);
      loadChannels(name);
      loadTools();
      loadSecrets();
    } catch (e) {
      setError(`${e}`);
    }
  }, [loadChannels, loadTools, loadSecrets]);

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
    (!form.hbEnabled || form.hbCron.trim().length > 0);

  const upd = (patch: Partial<FormState>) => setForm((f) => ({ ...f, ...patch }));

  const filteredAgents = agents.filter(
    (a) => !agentSearch || a.name.toLowerCase().includes(agentSearch.toLowerCase()),
  );
  // A search filter is active — an empty result here is "no matches", not onboarding.
  const isFiltered = agentSearch.trim() !== "";

  return (
    <PageContainer>
      <PageHeader
        title={t("agents.title")}
        description={t("agents.subtitle")}
        actions={
          <div className="flex flex-wrap items-center gap-2 w-full md:w-auto">
            <SearchInput
              value={agentSearch}
              onChange={setAgentSearch}
              placeholder={t("agents.search_placeholder")}
              className="flex-1 md:w-48"
            />
            <Button size="lg" onClick={openCreate} className="w-full md:w-auto gap-2">
              <Plus className="h-4 w-4" />
              {t("agents.new_agent")}
            </Button>
          </div>
        }
      />

      {error && <ErrorBanner error={error} />}

      {loading ? (
        <div className="grid grid-cols-1 gap-6 md:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4">
          {[1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-64 rounded-xl" />
          ))}
        </div>
      ) : filteredAgents.length === 0 && isFiltered ? (
        <EmptyState
          icon={Search}
          text={t("agents.no_matches")}
          height="h-64"
          className="rounded-2xl"
          hint={
            <Button variant="link" onClick={() => setAgentSearch("")} className="p-0 h-auto mt-1">
              {t("agents.reset_filters")}
            </Button>
          }
        />
      ) : agents.length === 0 ? (
        <EmptyState icon={Bot} text={t("agents.no_active_agents")} height="h-64" className="rounded-2xl" />
      ) : (
        <div className="grid grid-cols-1 gap-6 md:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4">
          {filteredAgents.map((a) => (
            <Card key={a.name} interactive className="group p-4 md:p-5 transition-all duration-300 overflow-hidden flex flex-col min-w-0">
              <div className="flex items-start gap-3 mb-4 min-w-0">
                <div className="relative shrink-0">
                  <IconTile tone="muted" size="lg" className="border-primary/30 shadow-inner group-hover:border-primary/50 transition-colors overflow-hidden">
                    {a.icon_url ? (
                      <>
                        {/* eslint-disable-next-line @next/next/no-img-element -- agent icons are tiny avatars from arbitrary sources (uploads, data URIs, external); next/Image's optimisation pipeline adds no value at this size */}
                        <img src={a.icon_url} alt={a.name} loading="lazy" className="h-full w-full object-cover" />
                      </>
                    ) : (
                      <span className="font-mono text-lg font-black text-primary/80 group-hover:text-primary transition-colors">
                        {a.name.charAt(0).toUpperCase()}
                      </span>
                    )}
                  </IconTile>
                  <div className={`absolute -bottom-0.5 -right-0.5 h-4 w-4 rounded-full border-2 border-background ${a.is_running ? "bg-success" : "bg-muted-foreground/50"}`} />
                </div>
                <div className="flex flex-col gap-0.5 min-w-0 flex-1">
                  <div className="flex items-center gap-2 min-w-0">
                    <h3 className="font-mono text-sm font-bold tracking-tight text-foreground truncate min-w-0">{a.name}</h3>
                    <StatusBadge status={a.is_running ? "running" : "inactive"} size="sm" className="gap-1 shrink-0">
                      <StatusDot status={a.is_running ? "success" : "muted"} pulse={a.is_running} className="h-1.5 w-1.5" />
                      {a.is_running ? t("agents.active") : t("agents.inactive")}
                    </StatusBadge>
                  </div>
                  <span className="text-xs text-muted-foreground truncate">{a.profile}</span>
                </div>
              </div>

              <div className="space-y-3 mb-4 flex-1">
                <InfoRow label={t("agents.profile")} value={a.profile} />
                {a.routing_count > 0 && (
                  <InfoRow label={t("agents.routing")} value={t("agents.routing_rules_count", { count: a.routing_count })} />
                )}
                <InfoRow label={t("agents.language")} value={(() => { const l = LANGUAGES.find((l) => l.value === a.language); return l?.labelKey ? t(l.labelKey) : l?.label ?? a.language; })()} />

                {a.has_heartbeat && (
                  <div className="flex flex-col gap-1 border-b border-border/50 py-2">
                    <span className="font-mono text-2xs uppercase tracking-widest text-muted-foreground-subtle">{t("agents.schedule")}</span>
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
                  className={`tap-target md:min-h-0 md:min-w-0 md:h-8 font-mono text-2xs uppercase tracking-wider ${a.base ? "col-span-2" : ""}`}
                >
                  <Settings className="h-3.5 w-3.5 mr-1" /> {t("agents.configure")}
                </Button>
                {!a.base && (
                  <Button
                    variant="outline-destructive"
                    onClick={() => setDeleteTarget(a.name)}
                    disabled={saving}
                    className="tap-target md:min-h-0 md:min-w-0 md:h-8 font-mono text-2xs uppercase tracking-wider"
                    aria-label={t("agents.delete_agent_aria", { name: a.name })}
                  >
                    {saving ? <Loader variant="circular" size="sm" /> : <LogOut className="h-3.5 w-3.5 rotate-90 mr-1" />}
                    {!saving && t("common.delete")}
                  </Button>
                )}
              </div>
            </Card>
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
        channels={channels}
        channelSaving={channelSaving}
        onOpenChannelDialog={openChannelDialog}
        onRestartChannel={restartChannel}
        onDeleteChannelRequest={(id) => setDeleteChannelId(id)}
        editingBase={editName ? agents.find((a) => a.name === editName)?.base ?? false : false}
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
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle className="text-destructive">{t("agents.delete_agent_title", { name: deleteTarget ?? "" })}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("agents.delete_agent_description")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={doDelete}>
              {t("common.delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </PageContainer>
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
    <div className="flex items-center justify-between border-b border-border/50 py-2.5 overflow-hidden">
      <span className="text-sm text-muted-foreground">{label}</span>
      <span
        className={`max-w-[60%] truncate text-right font-medium transition-colors group-hover:text-foreground ${mono ? "font-mono text-sm text-foreground/80" : "text-foreground text-sm"}`}
      >
        {value}
      </span>
    </div>
  );
}
