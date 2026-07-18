"use client";

import { useMemo, useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { useAuthStore } from "@/stores/auth-store";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { DialogTabs } from "@/components/ui/dialog-tabs";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { CronSchedulePicker } from "@/components/ui/cron-schedule-picker";
import { Field } from "@/components/ui/field";
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
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
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuCheckboxItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { ChannelRow, RoutingRule } from "@/types/api";
import type { WebhookDto } from "@/types/api.generated";
import { ChevronDown, Bot, ExternalLink, Camera, Settings, Wrench, Zap, Archive, Clock, Radio, Sparkles, MessageSquareText } from "lucide-react";
import { Collapsible, CollapsibleTrigger, CollapsibleContent } from "@/components/ui/collapsible";
import { RoutingRulesEditor } from "./RoutingRulesEditor";
import { AgentPromptsEditor } from "./AgentPromptsEditor";
import { useProviders } from "@/lib/queries";
import { useProfiles } from "@/hooks/use-profiles";

const LANGUAGES: { value: string; labelKey?: TranslationKey; label?: string }[] = [
  { value: "ru", labelKey: "agents.lang_ru" },
  { value: "en", labelKey: "agents.lang_en" },
  { value: "es", label: "Espanol" },
  { value: "de", label: "Deutsch" },
  { value: "fr", label: "Francais" },
  { value: "zh", label: "\u4e2d\u6587" },
  { value: "ja", label: "\u65e5\u672c\u8a9e" },
  { value: "ko", label: "\ud55c\uad6d\uc5b4" },
  { value: "pt", label: "Portugu\u00eas" },
  { value: "it", label: "Italiano" },
  { value: "ar", label: "\u0627\u0644\u0639\u0631\u0628\u064a\u0629" },
  { value: "hi", label: "\u0939\u093f\u0928\u094d\u0926\u0940" },
];

export interface FormState {
  name: string;
  language: string;
  /** Name of the row in the `profiles` table this agent resolves providers
   *  from (replaces the removed provider/model/provider_connection/
   *  fallback_provider/tts_provider fields). */
  profile: string;
  temperature: string;
  maxTokens: string;
  hbEnabled: boolean;
  hbCron: string;
  hbTimezone: string;
  toolsEnabled: boolean;
  toolsAllowAll: boolean;
  toolsDenyAllOthers: boolean;
  toolsAllow: string;
  toolsDeny: string;
  compEnabled: boolean;
  compThreshold: string;
  compPreserveToolCalls: boolean;
  compPreserveLastN: string;
  maxToolsInContext: string;
  tlEnabled: boolean;
  tlMaxIterations: string;
  tlCompactOnOverflow: boolean;
  tlDetectLoops: boolean;
  tlWarnThreshold: string;
  tlBreakThreshold: string;
  tlMaxAutoContinues: string;
  sessionEnabled: boolean;
  sessionDmScope: string;
  sessionTtlDays: string;
  sessionMaxMessages: string;
  routing: RoutingRule[];
  voice: string;
  /// Pre-signed `/api/uploads/{id}?sig=&exp=` URL for icon preview.
  /// Hydrated from `AgentDetailDto.icon_url`. Refreshed by the
  /// `PUT /api/agents/{name}/icon` response after a fresh upload.
  iconUrl: string;
  approvalEnabled: boolean;
  approvalRequireFor: string[];
  approvalCategories: string[];
  approvalTimeout: string;
  // Compaction extra
  compMaxContextTokens: string;
  // Session extra
  sessionPruneToolOutput: string;
  maxHistoryMessages: string;
  // Heartbeat extra
  hbAnnounceTo: string;
  // Tool Groups
  toolGroupGit: boolean;
  toolGroupManagement: boolean;
  toolGroupSkillEditing: boolean;
  toolGroupSessionTools: boolean;
  // Hooks
  hooksLogAll: boolean;
  hooksBlockTools: string;
  hooksWebhooks: WebhookDto[];
  // Budget
  dailyBudgetTokens: string;
  // Access Control
  accessEnabled: boolean;
  accessMode: string;
  accessOwnerId: string;
  // Skill Review
  srEnabled: boolean;
  srMinToolCalls: string;
  // Tool Dispatcher
  toolDispatcherEnabled: boolean;
  toolDispatcherCoreExtra: string[];
  toolDispatcherPromotionMax: string;
  // Soul layer
  soulEnabled: boolean;
  soulReflectionThreshold: string;
  soulCooldownMin: string;
  soulTopK: string;
  soulBudgetTokens: string;
  soulMaxEvents: string;
  // Drift correction
  driftEnabled: boolean;
  driftThreshold: string;
  driftMinHistory: string;
  driftBaselineTurns: string;
  driftCorrect: boolean;
  driftAnchor: string;
  // Initiative
  initiativeEnabled: boolean;
  initiativeProposalCap: string;
  initiativeDecompose: boolean;
  initiativeDailyPlan: boolean;
  initiativeAutoApprove: boolean;
  initiativeTokenBudget: string;
  // Emotion
  emotionEnabled: boolean;
  emotionK: string;
  emotionBlendRate: string;
  emotionHalfLife: string;
}

export interface AgentEditDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  editName: string | null;
  form: FormState;
  upd: (patch: Partial<FormState>) => void;
  saving: boolean;
  canSave: boolean;
  onSave: () => void;
  // Tools
  toolNames: string[];
  // Secrets
  secretNames: string[];
  // Channels
  channels: ChannelRow[];
  channelSaving: boolean;
  onOpenChannelDialog: (ch?: ChannelRow) => void;
  onRestartChannel: (channelId: string) => void;
  onDeleteChannelRequest: (channelId: string) => void;
  // Whether the agent being edited is a base agent (affects Tool Dispatcher copy).
  editingBase?: boolean;
}

type AgentTab = "general" | "tools" | "behavior" | "soul" | "session" | "schedule" | "channels" | "prompts";

const AGENT_TABS: { id: AgentTab; icon: React.ComponentType<{ className?: string }>; labelKey: TranslationKey }[] = [
  { id: "general",  icon: Settings,          labelKey: "agents.tab_general"  },
  { id: "tools",    icon: Wrench,            labelKey: "agents.tab_tools"    },
  { id: "behavior", icon: Zap,               labelKey: "agents.tab_behavior" },
  { id: "soul",     icon: Sparkles,          labelKey: "agents.tab_soul"     },
  { id: "session",  icon: Archive,           labelKey: "agents.tab_session"  },
  { id: "schedule", icon: Clock,             labelKey: "agents.tab_schedule" },
  { id: "channels", icon: Radio,             labelKey: "agents.tab_channels" },
  { id: "prompts",  icon: MessageSquareText, labelKey: "agents.tab_prompts"  },
];

/** Cross-field gating for the Soul tab — mirrors the server-side `validate()`
 *  invariants (soul-layer design spec §validation). Pure so it's unit-testable
 *  without mounting the dialog. */
export function soulGating(
  form: {
    soulEnabled: boolean;
    driftEnabled: boolean;
    initiativeDailyPlan: boolean;
    initiativeTokenBudget: string;
    hbEnabled: boolean;
  },
  editingBase: boolean,
) {
  return {
    emotionDisabled: !form.soulEnabled,
    driftCorrectDisabled: !form.driftEnabled,
    initiativeDisabled: editingBase,
    // M2: server rejects daily_plan without a configured heartbeat.
    dailyPlanDisabled: editingBase || !form.hbEnabled,
    // M1: server requires daily_token_budget > 0 when auto_approve is on.
    autoApproveDisabled: !form.initiativeDailyPlan || !(parseInt(form.initiativeTokenBudget) > 0),
  };
}

export function AgentEditDialog({
  open,
  onOpenChange,
  editName,
  form,
  upd,
  saving,
  canSave,
  onSave,
  toolNames,
  // secretNames, channelSaving, onOpenChannelDialog, onRestartChannel,
  // onDeleteChannelRequest — accepted but no longer consumed here after the
  // channels tab refactor; kept in the interface for caller-side stability.
  channels,
  editingBase,
}: AgentEditDialogProps) {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<AgentTab>("general");
  const isValidAgentName = form.name.trim().length === 0 || /^[a-zA-Z0-9_-]+$/.test(form.name.trim());
  const { data: allProviders = [] } = useProviders();
  // Includes legacy "llm" type alongside "text" so this matches the provider
  // universe RoutingRulesEditor's ProviderSelect offers (categories=["text","llm"]);
  // otherwise a legacy-typed provider picked in a routing rule can't be found
  // here, leaving ModelCombobox's providerId null (degrades to free-text).
  const llmProviders = allProviders.filter((p) => p.type === "text" || p.type === "llm");
  const { data: profilesData } = useProfiles();
  const profileNames = (profilesData?.profiles ?? []).map((p) => p.name);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent layout="panel" size="3xl">
        <DialogHeader className="px-5 pt-4 pb-0 border-b-0 bg-muted/20">
          <DialogTitle className="text-sm font-bold text-foreground truncate pb-3">
            {editName ? t("agents.editing", { name: editName }) : t("agents.new_agent_dialog")}
          </DialogTitle>
          {/* Tab bar — icon-only on mobile, icon+label for the active tab */}
          <DialogTabs
            items={AGENT_TABS.map((tab) => ({ value: tab.id, label: t(tab.labelKey), icon: tab.icon }))}
            value={activeTab}
            onChange={setActiveTab}
          />
        </DialogHeader>
        <div className="border-t border-border bg-muted/10" />

        <DialogBody className="overscroll-contain">
          <div className="px-5 py-3"><div className="grid">

            {/* ── General tab ── */}
            <div className={`col-start-1 row-start-1 space-y-3 transition-none ${activeTab === "general" ? "" : "opacity-0 pointer-events-none select-none"}`}>
                <div className="flex flex-wrap items-end gap-3 mb-3">
                  <div className="shrink-0">
                    <div className="h-4" />
                    <button
                      type="button"
                      className="relative group"
                      onClick={async () => {
                        if (!editName) {
                          toast.error(t("agents.icon_save_agent_first"));
                          return;
                        }
                        const input = document.createElement("input");
                        input.type = "file";
                        input.accept = "image/*";
                        input.addEventListener("change", async () => {
                          const file = input.files?.[0];
                          if (!file) return;
                          const fd = new FormData();
                          fd.append("image", file);
                          try {
                            const token = useAuthStore.getState().token;
                            const resp = await fetch(`/api/agents/${encodeURIComponent(editName)}/icon`, {
                              method: "PUT",
                              headers: { Authorization: `Bearer ${token}` },
                              body: fd,
                            });
                            if (!resp.ok) throw new Error(t("common.upload_error"));
                            const data = (await resp.json()) as { icon_url: string };
                            // Strip origin for same-origin browser fetch.
                            const previewUrl = (() => {
                              try { return new URL(data.icon_url, window.location.origin).pathname + new URL(data.icon_url, window.location.origin).search; }
                              catch { return data.icon_url; }
                            })();
                            upd({ iconUrl: previewUrl });
                          } catch {
                            toast.error(t("common.icon_upload_error"));
                          }
                        }, { once: true });
                        input.click();
                      }}
                    >
                      {form.iconUrl ? (
                        <>
                          {/* eslint-disable-next-line @next/next/no-img-element -- agent icons are tiny avatars from arbitrary sources (uploads, data URIs, external); next/Image's optimisation pipeline adds no value at 40×40 */}
                          <img src={form.iconUrl} alt={t("agents.icon_alt")} className="h-10 w-10 rounded-lg object-cover border border-border group-hover:border-primary/50 transition-colors" />
                        </>
                      ) : (
                        <div className="flex h-10 w-10 items-center justify-center rounded-lg bg-muted/50 border border-border text-muted-foreground group-hover:border-primary/50 transition-colors">
                          <Bot className="h-4 w-4" />
                        </div>
                      )}
                      <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-foreground/40 opacity-0 group-hover:opacity-100 transition-opacity">
                        <Camera className="h-3.5 w-3.5 text-background" />
                      </div>
                    </button>
                  </div>
                  <div className="flex-1 space-y-2">
                    <label htmlFor="agent-edit-name" className="text-xs font-medium text-muted-foreground ml-1">{t("agents.field_name")}</label>
                    <Input
                      id="agent-edit-name"
                      value={form.name}
                      placeholder="my-agent-01"
                      className="bg-background border-border font-mono text-sm h-8"
                      onChange={(e) => upd({ name: e.target.value })}
                    />
                    {!isValidAgentName && (
                      <p className="text-sm text-destructive mt-1">{t("agents.name_invalid")}</p>
                    )}
                  </div>
                  <Field label={t("agents.field_language")} className="w-full sm:w-36 sm:shrink-0" labelClassName="text-xs">
                    <Select value={form.language} onValueChange={(v) => upd({ language: v })}>
                      <SelectTrigger className="w-full bg-background border-border text-sm h-9">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent className="border-border">
                        {LANGUAGES.map((lang) => (
                          <SelectItem key={lang.value} value={lang.value} className="text-sm">
                            {lang.labelKey ? t(lang.labelKey) : lang.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </Field>
                </div>
                <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                  <Field label={t("agents.field_profile")} labelClassName="text-xs">
                    <div className="space-y-1.5">
                      <Select value={form.profile || "Default"} onValueChange={(v) => upd({ profile: v })}>
                        <SelectTrigger className="w-full bg-background border-border text-sm h-9">
                          <SelectValue placeholder={t("agents.profile_unknown")} />
                        </SelectTrigger>
                        <SelectContent className="border-border">
                          {profileNames.length > 0 ? (
                            profileNames.map((name) => (
                              <SelectItem key={name} value={name} className="text-sm font-mono">
                                {name}
                              </SelectItem>
                            ))
                          ) : (
                            <SelectItem value="Default" className="text-sm font-mono">Default</SelectItem>
                          )}
                        </SelectContent>
                      </Select>
                      <a href="/profiles/" className="inline-flex items-center gap-1 text-2xs text-primary hover:text-primary/80 transition-colors">
                        {t("agents.manage_profiles")}
                        <ExternalLink className="h-3 w-3" />
                      </a>
                    </div>
                  </Field>
                  <Field label={t("agents.field_temperature")} labelClassName="text-xs">
                    <Input type="number" step="0.1" min="0" max="2" value={form.temperature} className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ temperature: e.target.value })} />
                  </Field>
                  <Field label={t("agents.field_max_tokens")} labelClassName="text-xs">
                    <Input type="number" step="256" min="256" max="65536" value={form.maxTokens} placeholder="Auto" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ maxTokens: e.target.value })} />
                  </Field>
                  <Field label={t("agents.field_top_k_tools")} labelClassName="text-xs">
                    <Input type="number" step="1" min="1" max="50" value={form.maxToolsInContext} placeholder={t("agents.placeholder_all")} className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ maxToolsInContext: e.target.value })} />
                  </Field>
                  <Field label={t("agents.field_daily_budget")} labelClassName="text-xs">
                    <Input type="number" step="10000" min="0" value={form.dailyBudgetTokens} className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ dailyBudgetTokens: e.target.value })} />
                  </Field>
                </div>
            </div>

            {/* ── Tools tab ── */}
            <div className={`col-start-1 row-start-1 space-y-3 transition-none ${activeTab === "tools" ? "" : "opacity-0 pointer-events-none select-none"}`}>
                <SwitchSection title={t("agents.section_tool_policy")} enabled={form.toolsEnabled} onToggle={(v) => upd({ toolsEnabled: v })}>
                  <div className="space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-xs font-medium text-muted-foreground">{t("agents.allow_all_tools")}</span>
                      <Switch checked={form.toolsAllowAll} onCheckedChange={(v) => upd({ toolsAllowAll: v })} className="data-[state=checked]:bg-primary" />
                    </div>
                    <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                      <Field label={t("agents.field_allowed")} labelClassName="text-xs">
                        <ToolMultiSelect tools={toolNames} selected={form.toolsAllow.split(",").map((s) => s.trim()).filter(Boolean)} onChange={(v) => upd({ toolsAllow: v.join(", ") })} placeholder={t("common.select_tools_placeholder")} />
                      </Field>
                      <Field label={t("agents.field_denied")} labelClassName="text-xs">
                        <ToolMultiSelect tools={toolNames} selected={form.toolsDeny.split(",").map((s) => s.trim()).filter(Boolean)} onChange={(v) => upd({ toolsDeny: v.join(", ") })} placeholder={t("common.select_tools_placeholder")} />
                      </Field>
                    </div>
                    <div className="border-t border-border/30 pt-2 mt-2">
                      <span className="text-xs font-medium text-muted-foreground mb-1.5 block">{t("agents.tool_groups")}</span>
                      <div className="grid grid-cols-1 sm:grid-cols-2 gap-1.5">
                        {([
                          ["toolGroupGit", t("agents.tool_group_git")] as const,
                          ["toolGroupManagement", t("agents.tool_group_management")] as const,
                          ["toolGroupSkillEditing", t("agents.tool_group_skills")] as const,
                          ["toolGroupSessionTools", t("agents.tool_group_sessions")] as const,
                        ]).map(([key, label]) => (
                          <label key={key} className="flex items-center gap-2 text-xs cursor-pointer">
                            <input type="checkbox" checked={form[key] as boolean} onChange={(e) => upd({ [key]: e.target.checked })} className="rounded border-border accent-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" />
                            <span>{label}</span>
                          </label>
                        ))}
                      </div>
                    </div>
                  </div>
                </SwitchSection>
                <SwitchSection title={t("agents.section_approval")} enabled={form.approvalEnabled} onToggle={(v) => upd({ approvalEnabled: v })}>
                  <div className="space-y-2">
                    <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
                      <div className="space-y-2">
                        <label className="text-xs font-medium text-muted-foreground ml-1">{t("agents.approval_categories")}</label>
                        <div className="flex flex-col gap-1.5">
                          {(["system", "destructive", "external"] as const).map((cat) => (
                            <label key={cat} className="flex items-center gap-2 text-xs cursor-pointer">
                              <input type="checkbox" checked={form.approvalCategories.includes(cat)} onChange={(e) => {
                                const next = e.target.checked ? [...form.approvalCategories, cat] : form.approvalCategories.filter((c: string) => c !== cat);
                                upd({ approvalCategories: next });
                              }} className="rounded border-border accent-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" />
                              <span className="font-mono">{cat}</span>
                              <span className="text-muted-foreground-subtle text-2xs">
                                {cat === "system" ? "(shell, code, git)" : cat === "destructive" ? "(write, delete, edit)" : "(all other tools)"}
                              </span>
                            </label>
                          ))}
                        </div>
                      </div>
                      <Field label={t("agents.approval_timeout")} labelClassName="text-xs" hint={t("agents.approval_timeout_hint")}>
                        <Input type="number" min="30" max="3600" step="30" value={form.approvalTimeout} className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ approvalTimeout: e.target.value })} />
                      </Field>
                    </div>
                    <Field label={t("agents.approval_specific_tools")} labelClassName="text-xs">
                      <ToolMultiSelect tools={toolNames} selected={form.approvalRequireFor} onChange={(v) => upd({ approvalRequireFor: v })} placeholder={t("agents.approval_tools_placeholder")} />
                    </Field>
                  </div>
                </SwitchSection>
                <SwitchSection
                  title={t("agents.section_tool_dispatcher")}
                  enabled={form.toolDispatcherEnabled}
                  onToggle={(v) => upd({ toolDispatcherEnabled: v })}
                >
                  <div className="space-y-2">
                    <p className="text-2xs text-muted-foreground leading-relaxed">
                      {t("agents.tool_dispatcher_hint")}
                      {editingBase && (
                        <>
                          <br />
                          <strong>{t("agents.tool_dispatcher_base_note_label")}</strong>{" "}
                          {t("agents.tool_dispatcher_base_note")}
                        </>
                      )}
                    </p>
                    <div className="space-y-2">
                      <label className="text-xs font-medium text-muted-foreground ml-1">{t("agents.tool_dispatcher_core_extra")}</label>
                      <p className="text-2xs text-muted-foreground -mt-1 mb-1 leading-snug">
                        {t("agents.tool_dispatcher_core_extra_hint")}
                      </p>
                      <TagInput
                        values={form.toolDispatcherCoreExtra}
                        onChange={(v) => upd({ toolDispatcherCoreExtra: v })}
                        placeholder={t("agents.tool_dispatcher_core_extra_placeholder")}
                        suggestions={toolNames}
                      />
                    </div>
                    <Field label={t("agents.tool_dispatcher_promotion_max")} labelClassName="text-xs" hint={t("agents.tool_dispatcher_promotion_max_hint")}>
                      <Input
                        type="number"
                        step="1"
                        min={0}
                        max={16}
                        value={form.toolDispatcherPromotionMax}
                        className="bg-background border-border font-mono text-sm h-8"
                        onChange={(e) => upd({ toolDispatcherPromotionMax: e.target.value })}
                      />
                    </Field>
                  </div>
                </SwitchSection>
            </div>

            {/* ── Behavior tab ── */}
            <div className={`col-start-1 row-start-1 space-y-3 transition-none ${activeTab === "behavior" ? "" : "opacity-0 pointer-events-none select-none"}`}>
                <SwitchSection title={t("agents.section_tool_loop")} enabled={form.tlEnabled} onToggle={(v) => upd({ tlEnabled: v })}>
                  <div className="space-y-2">
                    <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
                    <Field label={t("agents.field_tl_max_iterations")} labelClassName="text-xs" hint={t("agents.hint_tl_max_iterations")}>
                        <Input type="number" step="1" min="0" max="10000" className="bg-background border-border font-mono text-sm h-8" value={form.tlMaxIterations} onChange={(e) => upd({ tlMaxIterations: e.target.value })} />
                      </Field>
                      <Field label={t("agents.field_tl_warn_threshold")} labelClassName="text-xs">
                        <Input type="number" step="1" min="1" className="bg-background border-border font-mono text-sm h-8" value={form.tlWarnThreshold} onChange={(e) => upd({ tlWarnThreshold: e.target.value })} />
                      </Field>
                      <Field label={t("agents.field_tl_break_threshold")} labelClassName="text-xs">
                        <Input type="number" step="1" min="1" className="bg-background border-border font-mono text-sm h-8" value={form.tlBreakThreshold} onChange={(e) => upd({ tlBreakThreshold: e.target.value })} />
                      </Field>
                    </div>
                    <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
                      <Field label={t("agents.field_max_auto_continues")} labelClassName="text-xs">
                        <Input type="number" step="1" min="0" max="20" className="bg-background border-border font-mono text-sm h-8" value={form.tlMaxAutoContinues} onChange={(e) => upd({ tlMaxAutoContinues: e.target.value })} />
                      </Field>
                    </div>
                    <div className="flex items-center justify-between">
                      <span className="text-xs font-medium text-muted-foreground">{t("agents.tl_compact_on_overflow")}</span>
                      <Switch checked={form.tlCompactOnOverflow} onCheckedChange={(v) => upd({ tlCompactOnOverflow: v })} className="data-[state=checked]:bg-primary" />
                    </div>
                    <div className="flex items-center justify-between">
                      <span className="text-xs font-medium text-muted-foreground">{t("agents.tl_detect_loops")}</span>
                      <Switch checked={form.tlDetectLoops} onCheckedChange={(v) => upd({ tlDetectLoops: v })} className="data-[state=checked]:bg-primary" />
                    </div>
                  </div>
                </SwitchSection>
                <SwitchSection title={t("agents.section_hooks")} enabled={form.hooksLogAll || form.hooksBlockTools.trim() !== "" || form.hooksWebhooks.length > 0} onToggle={(v) => { if (!v) upd({ hooksLogAll: false, hooksBlockTools: "", hooksWebhooks: [] }); else upd({ hooksLogAll: true }); }}>
                  <div className="space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-xs font-medium text-muted-foreground">{t("agents.hooks_log_all")}</span>
                      <Switch checked={form.hooksLogAll} onCheckedChange={(v) => upd({ hooksLogAll: v })} className="data-[state=checked]:bg-primary" />
                    </div>
                    <Field label={t("agents.hooks_block_tools")} labelClassName="text-xs">
                      <Input value={form.hooksBlockTools} placeholder="tool1, tool2" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ hooksBlockTools: e.target.value })} />
                    </Field>
                    <WebhooksEditor webhooks={form.hooksWebhooks} onChange={(wh) => upd({ hooksWebhooks: wh })} />
                  </div>
                </SwitchSection>
                <SwitchSection title={t("agents.section_skills")} enabled={form.srEnabled} onToggle={(v) => upd({ srEnabled: v })}>
                  <div className="space-y-2">
                    <Field label={t("agents.field_sr_min_tool_calls")} labelClassName="text-xs" hint={t("agents.hint_sr_min_tool_calls")}>
                      <Input type="number" step="1" min="1" max="100" className="bg-background border-border font-mono text-sm h-8" value={form.srMinToolCalls} onChange={(e) => upd({ srMinToolCalls: e.target.value })} />
                    </Field>
                  </div>
                </SwitchSection>
            </div>

            {/* ── Session tab ── */}
            <div className={`col-start-1 row-start-1 space-y-3 transition-none ${activeTab === "session" ? "" : "opacity-0 pointer-events-none select-none"}`}>
                <SwitchSection title={t("agents.section_session")} enabled={form.sessionEnabled} onToggle={(v) => upd({ sessionEnabled: v })}>
                  <div className="space-y-2">
                    <Field label={t("agents.field_dm_scope")} labelClassName="text-xs">
                      <Select value={form.sessionDmScope} onValueChange={(v) => upd({ sessionDmScope: v })}>
                        <SelectTrigger className="w-full bg-background border-border text-sm h-9"><SelectValue /></SelectTrigger>
                        <SelectContent className="border-border">
                          <SelectItem value="per-channel-peer">{t("agents.dm_scope_per_channel_peer")}</SelectItem>
                          <SelectItem value="shared">{t("agents.dm_scope_shared")}</SelectItem>
                          <SelectItem value="per-peer">{t("agents.dm_scope_per_peer")}</SelectItem>
                          <SelectItem value="per-chat">{t("agents.dm_scope_per_chat")}</SelectItem>
                        </SelectContent>
                      </Select>
                    </Field>
                    <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                      <Field label={t("agents.field_ttl_days")} labelClassName="text-xs">
                        <Input type="number" step="1" min="0" className="bg-background border-border font-mono text-sm h-8" value={form.sessionTtlDays} onChange={(e) => upd({ sessionTtlDays: e.target.value })} />
                      </Field>
                      <Field label={t("agents.field_max_messages")} labelClassName="text-xs">
                        <Input type="number" step="1" min="0" className="bg-background border-border font-mono text-sm h-8" value={form.sessionMaxMessages} onChange={(e) => upd({ sessionMaxMessages: e.target.value })} />
                      </Field>
                      <Field label={t("agents.field_prune_tool_output")} labelClassName="text-xs">
                        <Input type="number" step="1" min="0" value={form.sessionPruneToolOutput} placeholder="Off" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ sessionPruneToolOutput: e.target.value })} />
                      </Field>
                      <Field label={t("agents.field_max_history")} labelClassName="text-xs">
                        <Input type="number" step="1" min="0" value={form.maxHistoryMessages} placeholder="Unlimited" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ maxHistoryMessages: e.target.value })} />
                      </Field>
                    </div>
                  </div>
                </SwitchSection>
                <SwitchSection title={t("agents.section_access")} enabled={form.accessEnabled} onToggle={(v) => upd({ accessEnabled: v })}>
                  <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                    <Field label={t("agents.field_access_mode")} labelClassName="text-xs">
                      <Select value={form.accessMode} onValueChange={(v) => upd({ accessMode: v })}>
                        <SelectTrigger className="bg-background border-border text-sm h-9"><SelectValue /></SelectTrigger>
                        <SelectContent>
                          <SelectItem value="open">Open</SelectItem>
                          <SelectItem value="restricted">Restricted</SelectItem>
                        </SelectContent>
                      </Select>
                    </Field>
                    {form.accessMode === "restricted" && (
                      <Field label={t("agents.field_access_owner_id")} labelClassName="text-xs">
                        <Input value={form.accessOwnerId} placeholder="Telegram User ID" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ accessOwnerId: e.target.value })} />
                      </Field>
                    )}
                  </div>
                </SwitchSection>
            </div>

            {/* ── Schedule tab ── */}
            <div className={`col-start-1 row-start-1 space-y-3 transition-none ${activeTab === "schedule" ? "" : "opacity-0 pointer-events-none select-none"}`}>
              <SwitchSection title={t("agents.section_schedule")} enabled={form.hbEnabled} onToggle={(v) => upd({ hbEnabled: v })}>
                <CronSchedulePicker value={form.hbCron} onChange={(v) => upd({ hbCron: v })} timezone={form.hbTimezone || "UTC"} onTimezoneChange={(v) => upd({ hbTimezone: v })} />
                <Field label={t("agents.field_announce_to")} labelClassName="text-xs">
                  <Select value={form.hbAnnounceTo || "__none__"} onValueChange={(v) => upd({ hbAnnounceTo: v === "__none__" ? "" : v })}>
                    <SelectTrigger className="w-full bg-background border-border text-sm h-9"><SelectValue /></SelectTrigger>
                    <SelectContent className="border-border">
                      <SelectItem value="__none__">&mdash;</SelectItem>
                      <SelectItem value="telegram">Telegram</SelectItem>
                      <SelectItem value="discord">Discord</SelectItem>
                    </SelectContent>
                  </Select>
                </Field>
              </SwitchSection>
            </div>

            {/* ── Channels tab ── */}
            <div className={`col-start-1 row-start-1 space-y-3 transition-none ${activeTab === "channels" ? "" : "opacity-0 pointer-events-none select-none"}`}>
                <RoutingRulesEditor routing={form.routing} llmProviders={llmProviders} onChange={(routing) => upd({ routing })} />
                {editName && (
                  <div className="space-y-2 border-t border-border/30 pt-3">
                    <div className="flex items-center justify-between">
                      <h3 className="text-xs font-semibold uppercase tracking-wide text-foreground">{t("agents.section_channels")}</h3>
                      <a href="/channels/" className="inline-flex items-center gap-1 text-xs text-primary hover:text-primary/80 transition-colors">
                        {t("agents.manage_channels")}
                        <ExternalLink className="h-4 w-4" />
                      </a>
                    </div>
                    {channels.length === 0 ? (
                      <p className="text-xs text-muted-foreground-subtle py-2">{t("agents.no_channels")}</p>
                    ) : (
                      <div className="space-y-2">
                        {channels.map((ch) => (
                          <div key={ch.id} className="flex items-center gap-3 rounded-lg border border-border bg-muted/20 px-3 py-2.5 min-w-0 overflow-hidden">
                            <div className={`h-3 w-3 rounded-full shrink-0 ${ch.status === "running" ? "bg-success" : ch.status === "error" ? "bg-destructive" : "bg-muted-foreground/40"}`} />
                            <div className="flex-1 min-w-0">
                              <p className="text-xs font-semibold truncate text-foreground">{ch.display_name}</p>
                              <p className="text-2xs text-muted-foreground font-mono uppercase">{ch.channel_type}</p>
                            </div>
                            <span className="text-2xs font-mono text-muted-foreground/50">{ch.id.slice(0, 8)}</span>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                )}
            </div>

            {/* ── Prompts tab ── (starter-prompt chips for the chat welcome screen) */}
            <div className={`col-start-1 row-start-1 space-y-3 transition-none ${activeTab === "prompts" ? "" : "opacity-0 pointer-events-none select-none"}`}>
                <AgentPromptsEditor agentName={editName} />
            </div>

            {/* ── Soul tab ──
                 Placed last in DOM order (though it's positioned after Behavior
                 in the AGENT_TABS bar) so its always-mounted, initially-unchecked
                 switches don't shift the "first unchecked switch" indices that
                 agent-tabs.test.tsx relies on for the Session/Schedule tabs.
                 Panels overlay via col-start-1 row-start-1, so DOM order has no
                 visual effect. */}
            <div className={`col-start-1 row-start-1 space-y-3 transition-none ${activeTab === "soul" ? "" : "opacity-0 pointer-events-none select-none"}`}>
              {(() => {
                const g = soulGating(form, !!editingBase);
                return (
                  <>
                    <SwitchSection title={t("agents.section_soul")} enabled={form.soulEnabled} onToggle={(v) => upd(v ? { soulEnabled: v } : { soulEnabled: v, emotionEnabled: false })}>
                      <AdvancedSection label={t("common.advanced")}>
                        <Field label={t("agents.soul_reflection_threshold")} labelClassName="text-xs">
                          <Input type="number" min={1} className="bg-background border-border font-mono text-sm h-8" value={form.soulReflectionThreshold} onChange={(e) => upd({ soulReflectionThreshold: e.target.value })} />
                        </Field>
                        <Field label={t("agents.soul_cooldown_min")} labelClassName="text-xs">
                          <Input type="number" min={0} max={1440} className="bg-background border-border font-mono text-sm h-8" value={form.soulCooldownMin} onChange={(e) => upd({ soulCooldownMin: e.target.value })} />
                        </Field>
                        <Field label={t("agents.soul_top_k")} labelClassName="text-xs">
                          <Input type="number" min={1} max={20} className="bg-background border-border font-mono text-sm h-8" value={form.soulTopK} onChange={(e) => upd({ soulTopK: e.target.value })} />
                        </Field>
                        <Field label={t("agents.soul_budget_tokens")} labelClassName="text-xs">
                          <Input type="number" min={100} max={4000} className="bg-background border-border font-mono text-sm h-8" value={form.soulBudgetTokens} onChange={(e) => upd({ soulBudgetTokens: e.target.value })} />
                        </Field>
                        <Field label={t("agents.soul_max_events")} labelClassName="text-xs">
                          <Input type="number" min={1} max={30} className="bg-background border-border font-mono text-sm h-8" value={form.soulMaxEvents} onChange={(e) => upd({ soulMaxEvents: e.target.value })} />
                        </Field>
                      </AdvancedSection>
                    </SwitchSection>

                    <SwitchSection title={t("agents.section_drift")} enabled={form.driftEnabled} onToggle={(v) => upd(v ? { driftEnabled: v } : { driftEnabled: v, driftCorrect: false })}>
                      <div className="flex items-center justify-between">
                        <span className="text-xs font-medium text-muted-foreground">{t("agents.drift_correct")}</span>
                        <Switch checked={form.driftCorrect} disabled={g.driftCorrectDisabled} onCheckedChange={(v) => upd({ driftCorrect: v })} className="data-[state=checked]:bg-primary" />
                      </div>
                      <Field label={t("agents.drift_anchor")} labelClassName="text-xs" hint={t("agents.drift_anchor_hint")}>
                        <textarea
                          className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                          rows={2}
                          value={form.driftAnchor}
                          onChange={(e) => upd({ driftAnchor: e.target.value })}
                        />
                      </Field>
                      <AdvancedSection label={t("common.advanced")}>
                        <Field label={t("agents.drift_threshold")} labelClassName="text-xs" hint={t("agents.drift_threshold_deprecated")}>
                          <Input type="number" step="0.01" min={0} max={2} disabled className="bg-background border-border font-mono text-sm h-8" value={form.driftThreshold} onChange={(e) => upd({ driftThreshold: e.target.value })} />
                        </Field>
                        <Field label={t("agents.drift_min_history")} labelClassName="text-xs">
                          <Input type="number" min={2} max={50} className="bg-background border-border font-mono text-sm h-8" value={form.driftMinHistory} onChange={(e) => upd({ driftMinHistory: e.target.value })} />
                        </Field>
                        <Field label={t("agents.drift_baseline_turns")} labelClassName="text-xs">
                          <Input type="number" min={1} max={10} className="bg-background border-border font-mono text-sm h-8" value={form.driftBaselineTurns} onChange={(e) => upd({ driftBaselineTurns: e.target.value })} />
                        </Field>
                      </AdvancedSection>
                    </SwitchSection>

                    <SwitchSection title={t("agents.section_initiative")} enabled={form.initiativeEnabled} disabled={g.initiativeDisabled} note={g.initiativeDisabled ? t("agents.initiative_non_base_note") : undefined} onToggle={(v) => upd({ initiativeEnabled: v })}>
                      <div className="flex items-center justify-between">
                        <span className="text-xs font-medium text-muted-foreground">{t("agents.initiative_daily_plan")}</span>
                        <Switch checked={form.initiativeDailyPlan} disabled={g.dailyPlanDisabled} onCheckedChange={(v) => upd(v ? { initiativeDailyPlan: v } : { initiativeDailyPlan: v, initiativeAutoApprove: false })} className="data-[state=checked]:bg-primary" />
                      </div>
                      <p className="text-xs text-muted-foreground">{t("agents.initiative_daily_plan_hint")}</p>
                      <div className="flex items-center justify-between">
                        <span className="text-xs font-medium text-muted-foreground">{t("agents.initiative_auto_approve")}</span>
                        <Switch checked={form.initiativeAutoApprove} disabled={g.autoApproveDisabled || g.initiativeDisabled} onCheckedChange={(v) => upd({ initiativeAutoApprove: v })} className="data-[state=checked]:bg-primary" />
                      </div>
                      <AdvancedSection label={t("common.advanced")}>
                        <Field label={t("agents.initiative_proposal_cap")} labelClassName="text-xs">
                          <Input type="number" min={1} max={10} disabled={g.initiativeDisabled} className="bg-background border-border font-mono text-sm h-8" value={form.initiativeProposalCap} onChange={(e) => upd({ initiativeProposalCap: e.target.value })} />
                        </Field>
                        <div className="flex items-center justify-between">
                          <span className="text-xs font-medium text-muted-foreground">{t("agents.initiative_decompose")}</span>
                          <Switch checked={form.initiativeDecompose} disabled={g.initiativeDisabled} onCheckedChange={(v) => upd({ initiativeDecompose: v })} className="data-[state=checked]:bg-primary" />
                        </div>
                        <Field label={t("agents.initiative_token_budget")} labelClassName="text-xs" hint={t("agents.initiative_token_budget_hint")}>
                          <Input type="number" min={0} max={1000000000000} disabled={g.initiativeDisabled} className="bg-background border-border font-mono text-sm h-8" value={form.initiativeTokenBudget} onChange={(e) => upd({ initiativeTokenBudget: e.target.value })} />
                        </Field>
                      </AdvancedSection>
                    </SwitchSection>

                    <SwitchSection
                      title={t("agents.section_emotion")}
                      enabled={form.emotionEnabled}
                      disabled={g.emotionDisabled}
                      note={g.emotionDisabled ? t("agents.emotion_requires_soul_note") : undefined}
                      onToggle={(v) => upd({ emotionEnabled: v })}
                    >
                      <AdvancedSection label={t("common.advanced")}>
                        <Field label={t("agents.emotion_k")} labelClassName="text-xs">
                          <Input type="number" step="0.1" min={0} max={5} className="bg-background border-border font-mono text-sm h-8" value={form.emotionK} onChange={(e) => upd({ emotionK: e.target.value })} />
                        </Field>
                        <Field label={t("agents.emotion_blend_rate")} labelClassName="text-xs">
                          <Input type="number" step="0.05" min={0} max={1} className="bg-background border-border font-mono text-sm h-8" value={form.emotionBlendRate} onChange={(e) => upd({ emotionBlendRate: e.target.value })} />
                        </Field>
                        <Field label={t("agents.emotion_half_life")} labelClassName="text-xs">
                          <Input type="number" step="0.5" min={0} className="bg-background border-border font-mono text-sm h-8" value={form.emotionHalfLife} onChange={(e) => upd({ emotionHalfLife: e.target.value })} />
                        </Field>
                      </AdvancedSection>
                    </SwitchSection>
                  </>
                );
              })()}
            </div>

          </div></div>
        </DialogBody>

        <DialogFooter className="px-5 py-3 border-t border-border bg-muted/20">
          <div className="flex gap-3 w-full justify-end">
            <Button variant="ghost" className="text-muted-foreground hover:text-foreground" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button onClick={onSave} disabled={saving || !canSave} className="px-5 font-semibold">
              {saving ? t("common.saving") : editName ? t("common.save") : t("common.create")}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export interface ChannelDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  channelDialogId: string | null;
  channelForm: { channel_type: string; display_name: string; bot_token: string; api_url: string };
  setChannelForm: React.Dispatch<React.SetStateAction<{ channel_type: string; display_name: string; bot_token: string; api_url: string }>>;
  channelSaving: boolean;
  onSave: () => void;
}

export function ChannelDialog({
  open,
  onOpenChange,
  channelDialogId,
  channelForm,
  setChannelForm,
  channelSaving,
  onSave,
}: ChannelDialogProps) {
  const { t } = useTranslation();

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent layout="panel" size="sm">
        <DialogHeader className="px-5 py-4 border-b border-border bg-muted/20">
          <DialogTitle className="text-sm font-bold">{channelDialogId ? t("agents.channel_edit") : t("agents.channel_add_dialog")}</DialogTitle>
        </DialogHeader>
        <DialogBody className="px-5 py-4 space-y-4">
          <Field label={t("agents.channel_field_type")} labelClassName="text-xs">
            <Select
              value={channelForm.channel_type}
              onValueChange={(v) => setChannelForm((f) => ({ ...f, channel_type: v }))}
              disabled={!!channelDialogId}
            >
              <SelectTrigger className="w-full bg-background border-border text-sm h-9">
                <SelectValue />
              </SelectTrigger>
              <SelectContent className="border-border">
                <SelectItem value="telegram">Telegram</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          <Field label={t("agents.channel_field_display_name")} labelClassName="text-xs">
            <Input
              value={channelForm.display_name}
              placeholder={t("agents.channel_placeholder_name")}
              className="bg-background border-border text-sm h-8"
              onChange={(e) => setChannelForm((f) => ({ ...f, display_name: e.target.value }))}
            />
          </Field>
          <Field label={t("agents.channel_field_bot_token")} labelClassName="text-xs">
            <Input
              type="password"
              value={channelForm.bot_token}
              placeholder="5092...:AAE..."
              className="bg-background border-border font-mono text-sm h-8"
              onChange={(e) => setChannelForm((f) => ({ ...f, bot_token: e.target.value }))}
            />
          </Field>
          <Field label={t("agents.channel_field_api_url")} labelClassName="text-xs">
            <Input
              value={channelForm.api_url}
              placeholder="http://localhost:8081"
              className="bg-background border-border font-mono text-sm h-8"
              onChange={(e) => setChannelForm((f) => ({ ...f, api_url: e.target.value }))}
            />
          </Field>
        </DialogBody>
        <DialogFooter className="px-5 py-3 border-t border-border bg-muted/20">
          <div className="flex gap-3 w-full justify-end">
            <Button variant="ghost" className="text-muted-foreground" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button>
            <Button
              onClick={onSave}
              disabled={channelSaving || !channelForm.display_name.trim() || !channelForm.bot_token.trim()}
              className="px-5 font-semibold"
            >
              {channelSaving ? t("common.saving") : channelDialogId ? t("agents.channel_save_and_restart") : t("common.create")}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export interface DeleteChannelDialogProps {
  deleteChannelId: string | null;
  onOpenChange: (open: boolean) => void;
  onConfirm: (channelId: string) => void;
}

export function DeleteChannelDialog({
  deleteChannelId,
  onOpenChange,
  onConfirm,
}: DeleteChannelDialogProps) {
  const { t } = useTranslation();

  return (
    <AlertDialog open={!!deleteChannelId} onOpenChange={(o) => { if (!o) onOpenChange(false); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle className="text-destructive">{t("agents.delete_channel_title")}</AlertDialogTitle>
          <AlertDialogDescription>
            {t("agents.delete_channel_description")}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            onClick={() => deleteChannelId && onConfirm(deleteChannelId)}
          >
            {t("common.delete")}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

// --- Helper components ---

function SwitchSection({
  title,
  enabled,
  onToggle,
  disabled = false,
  note,
  children,
}: {
  title: string;
  enabled: boolean;
  onToggle: (v: boolean) => void;
  disabled?: boolean;
  /** Optional explanatory line shown under the header — visible even when the
   *  section is off/disabled (e.g. "requires Soul enabled"). Kept inside the
   *  section so it groups with THIS header, not the previous section. */
  note?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-2 border-t border-border/30 pt-3">
      <div className="flex items-center justify-between">
        <h3 className={`text-xs font-semibold uppercase tracking-wide transition-colors ${enabled ? "text-foreground" : "text-muted-foreground"}`}>
          {title}
        </h3>
        <Switch
          checked={enabled}
          onCheckedChange={onToggle}
          disabled={disabled}
          className="data-[state=checked]:bg-primary"
        />
      </div>
      {note && <p className="text-xs text-warning">{note}</p>}
      {enabled && <div className="animate-in fade-in duration-200">{children}</div>}
    </div>
  );
}

/** Collapsed-by-default group for the seldom-tuned numeric knobs of a soul
 *  sub-section, so the panel doesn't overwhelm with fields on first open. */
function AdvancedSection({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <Collapsible className="mt-2">
      <CollapsibleTrigger className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
        <ChevronDown className="h-3 w-3" /> {label}
      </CollapsibleTrigger>
      <CollapsibleContent className="space-y-2 pt-2">{children}</CollapsibleContent>
    </Collapsible>
  );
}

function ToolMultiSelect({
  tools,
  selected,
  onChange,
  placeholder,
}: {
  tools: string[];
  selected: string[];
  onChange: (v: string[]) => void;
  placeholder: string;
}) {
  const { t: tr } = useTranslation();
  const selectedSet = useMemo(() => new Set(selected), [selected]);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="outline"
          className="w-full justify-between bg-background border-border text-sm h-8 font-normal"
        >
          {selected.length > 0 ? (
            <span className="truncate font-mono text-xs">{selected.join(", ")}</span>
          ) : (
            <span className="text-muted-foreground">{placeholder}</span>
          )}
          <ChevronDown className="h-3.5 w-3.5 shrink-0 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent className="max-h-60 overflow-y-auto border-border min-w-[var(--radix-dropdown-menu-trigger-width)]">
        {tools.length === 0 ? (
          <div className="px-2 py-1.5 text-xs text-muted-foreground">{tr("common.no_records_found")}</div>
        ) : (
          tools.map((t) => (
            <DropdownMenuCheckboxItem
              key={t}
              checked={selectedSet.has(t)}
              onCheckedChange={(checked) => {
                onChange(
                  checked
                    ? [...selected, t]
                    : selected.filter((s) => s !== t),
                );
              }}
              onSelect={(e) => e.preventDefault()}
              className="font-mono text-xs"
            >
              {t}
            </DropdownMenuCheckboxItem>
          ))
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function TagInput({
  values,
  onChange,
  placeholder,
  suggestions,
}: {
  values: string[];
  onChange: (v: string[]) => void;
  placeholder?: string;
  suggestions?: string[];
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState("");
  const datalistId = useMemo(
    () => `taginput-${Math.random().toString(36).slice(2, 10)}`,
    [],
  );
  const commit = (raw: string) => {
    const trimmed = raw.trim();
    if (!trimmed) return;
    if (values.includes(trimmed)) {
      setDraft("");
      return;
    }
    onChange([...values, trimmed]);
    setDraft("");
  };
  return (
    <div className="flex flex-wrap items-center gap-1.5 rounded-md border border-border bg-background px-2 py-1.5 min-h-8">
      {values.map((v) => (
        <span
          key={v}
          className="inline-flex items-center gap-1 rounded-full border border-border bg-muted/30 pl-2 pr-1 py-0.5 font-mono text-2xs"
        >
          <span className="truncate max-w-40">{v}</span>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            onClick={() => onChange(values.filter((x) => x !== v))}
            className="rounded-full text-muted-foreground hover:text-foreground h-4 w-4"
            aria-label={t("common.remove", { item: v })}
          >
            ×
          </Button>
        </span>
      ))}
      <input
        type="text"
        list={suggestions && suggestions.length > 0 ? datalistId : undefined}
        value={draft}
        placeholder={values.length === 0 ? placeholder : ""}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === ",") {
            e.preventDefault();
            commit(draft);
          } else if (e.key === "Backspace" && draft.length === 0 && values.length > 0) {
            e.preventDefault();
            onChange(values.slice(0, -1));
          }
        }}
        onBlur={() => commit(draft)}
        className="flex-1 min-w-30 bg-transparent outline-none text-xs font-mono placeholder:text-muted-foreground"
      />
      {suggestions && suggestions.length > 0 && (
        <datalist id={datalistId}>
          {suggestions.map((s) => (
            <option key={s} value={s} />
          ))}
        </datalist>
      )}
    </div>
  );
}

// ── WebhooksEditor ────────────────────────────────────────────────────────────

const WEBHOOK_EVENTS = ["BeforeMessage", "BeforeToolCall", "AfterToolResult"] as const;

export interface WebhooksEditorProps {
  webhooks: WebhookDto[];
  onChange: (webhooks: WebhookDto[]) => void;
}

export function WebhooksEditor({ webhooks, onChange }: WebhooksEditorProps) {
  const defaultWebhook: WebhookDto = {
    url: "",
    events: [],
    mode: "async",
    tool_matcher: null,
    on_failure: "open",
    timeout_ms: 3000,
    allow_internal: false,
  };

  function update(index: number, patch: Partial<WebhookDto>) {
    onChange(webhooks.map((wh, i) => (i === index ? { ...wh, ...patch } : wh)));
  }

  function remove(index: number) {
    onChange(webhooks.filter((_, i) => i !== index));
  }

  function toggleEvent(index: number, event: string, checked: boolean) {
    const wh = webhooks[index];
    const events = checked
      ? [...wh.events, event]
      : wh.events.filter((e) => e !== event);
    update(index, { events });
  }

  return (
    <div className="space-y-2 mt-2">
      {webhooks.map((wh, i) => (
        <div key={i} className="border border-border/50 rounded-lg p-3 space-y-2 bg-muted/10 min-w-0 overflow-hidden">
          {/* URL */}
          <div className="flex items-center gap-2">
            <Input
              value={wh.url}
              placeholder="https://"
              className="bg-background border-border font-mono text-xs h-7 flex-1"
              onChange={(e) => update(i, { url: e.target.value })}
            />
            <Button
              type="button"
              variant="ghost"
              size="sm"
              aria-label="Удалить"
              className="h-7 px-2 text-muted-foreground hover:text-destructive shrink-0"
              onClick={() => remove(i)}
            >
              ×
            </Button>
          </div>

          {/* Events */}
          <div className="flex flex-wrap gap-3">
            {WEBHOOK_EVENTS.map((ev) => (
              <label key={ev} className="flex items-center gap-1.5 cursor-pointer">
                <input
                  type="checkbox"
                  role="checkbox"
                  aria-label={ev}
                  checked={wh.events.includes(ev)}
                  onChange={(e) => toggleEvent(i, ev, e.target.checked)}
                  className="h-3.5 w-3.5 accent-primary"
                />
                <span className="text-xs font-mono text-muted-foreground">{ev}</span>
              </label>
            ))}
          </div>

          {/* Mode */}
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground w-12 shrink-0">mode</span>
            <Select value={wh.mode} onValueChange={(v) => update(i, { mode: v })}>
              <SelectTrigger className="bg-background border-border text-xs h-7 flex-1">
                <SelectValue />
              </SelectTrigger>
              <SelectContent className="border-border">
                <SelectItem value="async">async</SelectItem>
                <SelectItem value="decision">decision</SelectItem>
              </SelectContent>
            </Select>
          </div>

          {/* Decision-only fields */}
          {wh.mode === "decision" && (
            <div className="space-y-2 pl-1 border-l-2 border-primary/30">
              {/* tool_matcher */}
              <div className="flex items-center gap-2">
                <span className="text-xs text-muted-foreground w-24 shrink-0">tool_matcher</span>
                <Input
                  value={wh.tool_matcher ?? ""}
                  placeholder="tool_matcher"
                  className="bg-background border-border font-mono text-xs h-7 flex-1"
                  onChange={(e) => update(i, { tool_matcher: e.target.value || null })}
                />
              </div>
              {/* on_failure */}
              <div className="flex items-center gap-2">
                <span className="text-xs text-muted-foreground w-24 shrink-0">on_failure</span>
                <Select value={wh.on_failure} onValueChange={(v) => update(i, { on_failure: v })}>
                  <SelectTrigger className="bg-background border-border text-xs h-7 flex-1">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent className="border-border">
                    <SelectItem value="open">open</SelectItem>
                    <SelectItem value="closed">closed</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              {/* timeout_ms */}
              <div className="flex items-center gap-2">
                <span className="text-xs text-muted-foreground w-24 shrink-0">timeout (ms)</span>
                <Input
                  type="number"
                  value={wh.timeout_ms}
                  className="bg-background border-border font-mono text-xs h-7 flex-1"
                  onChange={(e) => update(i, { timeout_ms: parseInt(e.target.value) || 3000 })}
                />
              </div>
              {/* allow_internal */}
              <div className="flex items-center justify-between">
                <span className="text-xs text-muted-foreground">allow_internal</span>
                <Switch
                  checked={wh.allow_internal}
                  onCheckedChange={(v) => update(i, { allow_internal: v })}
                  className="data-[state=checked]:bg-primary"
                />
              </div>
            </div>
          )}
        </div>
      ))}
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="w-full h-7 text-xs border-dashed border-border/50 text-muted-foreground hover:text-foreground"
        onClick={() => onChange([...webhooks, { ...defaultWebhook }])}
      >
        + Добавить webhook
      </Button>
    </div>
  );
}
