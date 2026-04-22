"use client";

import { useMemo, useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { useAuthStore } from "@/stores/auth-store";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { ScrollArea } from "@/components/ui/scroll-area";
import { CronSchedulePicker } from "@/components/ui/cron-schedule-picker";
import {
  Dialog,
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
import type { ChannelRow, Provider, RoutingRule } from "@/types/api";
import { ChevronDown, Bot, ExternalLink, Link2, Camera, RefreshCw, Settings, Wrench, Zap, Archive, Clock, Radio } from "lucide-react";
import { RoutingRulesEditor } from "./RoutingRulesEditor";
import { useProviders, useProviderModels } from "@/lib/queries";

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
  provider: string;
  model: string;
  providerConnection: string;
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
  icon: string;
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
  // Budget
  dailyBudgetTokens: string;
  // Access Control
  accessEnabled: boolean;
  accessMode: string;
  accessOwnerId: string;
  // Fallback provider
  fallbackProvider: string;
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
  // Models (used by RoutingRulesEditor)
  discoveredModels: Record<string, string[]>;
  modelsLoading?: string | null;
  fetchModels: (provider: string) => void;
  // Tools
  toolNames: string[];
  // Secrets
  secretNames: string[];
  // Voices
  voices: { id: string; name: string; description?: string }[];
  // Channels
  channels: ChannelRow[];
  channelSaving: boolean;
  onOpenChannelDialog: (ch?: ChannelRow) => void;
  onRestartChannel: (channelId: string) => void;
  onDeleteChannelRequest: (channelId: string) => void;
}

type AgentTab = "general" | "tools" | "behavior" | "session" | "schedule" | "channels";

const AGENT_TABS: { id: AgentTab; icon: React.ComponentType<{ className?: string }>; labelKey: TranslationKey }[] = [
  { id: "general",  icon: Settings, labelKey: "agents.tab_general"  },
  { id: "tools",    icon: Wrench,   labelKey: "agents.tab_tools"    },
  { id: "behavior", icon: Zap,      labelKey: "agents.tab_behavior" },
  { id: "session",  icon: Archive,  labelKey: "agents.tab_session"  },
  { id: "schedule", icon: Clock,    labelKey: "agents.tab_schedule" },
  { id: "channels", icon: Radio,    labelKey: "agents.tab_channels" },
];

export function AgentEditDialog({
  open,
  onOpenChange,
  editName,
  form,
  upd,
  saving,
  canSave,
  onSave,
  discoveredModels,
  fetchModels,
  toolNames,
  secretNames,
  voices,
  channels,
  channelSaving,
  onOpenChannelDialog,
  onRestartChannel,
  onDeleteChannelRequest,
}: AgentEditDialogProps) {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<AgentTab>("general");
  const isValidAgentName = form.name.trim().length === 0 || /^[a-zA-Z0-9_-]+$/.test(form.name.trim());
  const { data: allProviders = [] } = useProviders();
  const llmProviders = allProviders.filter((p) => p.type === "text");
  const selectedProvider = llmProviders.find((p) => p.name === form.providerConnection);
  const { data: providerModels = [], isLoading: providerModelsLoading, refetch: refetchModels } = useProviderModels(selectedProvider?.id ?? null);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-hidden p-0 border-border shadow-2xl max-w-[calc(100%-1rem)] sm:max-w-2xl rounded-xl">
        <DialogHeader className="px-5 pt-4 pb-0 border-b-0 bg-muted/20">
          <DialogTitle className="text-sm font-bold text-foreground truncate pb-3">
            {editName ? t("agents.editing", { name: editName }) : t("agents.new_agent_dialog")}
          </DialogTitle>
          {/* Tab bar — icons only on mobile, icons+labels on sm+ */}
          <div className="flex gap-0.5 overflow-x-auto scrollbar-none -mx-5 px-5 pb-0">
            {AGENT_TABS.map((tab) => {
              const Icon = tab.icon;
              const isActive = activeTab === tab.id;
              return (
                <button
                  key={tab.id}
                  type="button"
                  onClick={() => setActiveTab(tab.id)}
                  title={t(tab.labelKey)}
                  className={`
                    relative flex items-center gap-1.5 px-2.5 sm:px-3 py-2 text-xs font-medium whitespace-nowrap
                    transition-colors rounded-t-md shrink-0
                    ${isActive
                      ? "text-foreground bg-background border-t border-l border-r border-border"
                      : "text-muted-foreground hover:text-foreground hover:bg-muted/30"
                    }
                  `}
                >
                  <Icon className="h-3.5 w-3.5 shrink-0 sm:h-3 sm:w-3" />
                  <span className="hidden sm:inline">{t(tab.labelKey)}</span>
                  {isActive && (
                    <span className="absolute bottom-0 left-0 right-0 h-px bg-background" />
                  )}
                </button>
              );
            })}
          </div>
        </DialogHeader>
        <div className="border-t border-border bg-muted/5" />

        <ScrollArea className="max-h-[55vh] sm:max-h-[65vh]">
          <div className="px-5 py-3 space-y-3">

            {/* ── General tab ── */}
            {activeTab === "general" && (
              <>
                <div className="flex flex-wrap items-end gap-3 mb-3">
                  <div className="shrink-0">
                    <div className="h-[18px]" />
                    <button
                      type="button"
                      className="relative group"
                      onClick={async () => {
                        const input = document.createElement("input");
                        input.type = "file";
                        input.accept = "image/*";
                        input.addEventListener("change", async () => {
                          const file = input.files?.[0];
                          if (!file) return;
                          const fd = new FormData();
                          fd.append("file", file);
                          try {
                            const token = useAuthStore.getState().token;
                            const resp = await fetch("/api/media/upload", {
                              method: "POST",
                              headers: { Authorization: `Bearer ${token}` },
                              body: fd,
                            });
                            if (!resp.ok) throw new Error(t("common.upload_error"));
                            const data = await resp.json();
                            const filename = data.filename || data.url?.split("/").pop();
                            if (filename) upd({ icon: filename });
                          } catch {
                            toast.error(t("common.icon_upload_error"));
                          }
                        }, { once: true });
                        input.click();
                      }}
                    >
                      {form.icon ? (
                        <img src={`/uploads/${form.icon}`} alt={t("agents.icon_alt")} className="h-10 w-10 rounded-lg object-cover border border-border group-hover:border-primary/50 transition-colors" />
                      ) : (
                        <div className="flex h-10 w-10 items-center justify-center rounded-lg bg-muted/50 border border-border text-muted-foreground group-hover:border-primary/50 transition-colors">
                          <Bot className="h-4 w-4" />
                        </div>
                      )}
                      <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-black/40 opacity-0 group-hover:opacity-100 transition-opacity">
                        <Camera className="h-3.5 w-3.5 text-white" />
                      </div>
                    </button>
                  </div>
                  <Field label={t("agents.field_name")} className="flex-1">
                    <Input
                      value={form.name}
                      placeholder="my-agent-01"
                      className="bg-background border-border font-mono text-sm h-8"
                      onChange={(e) => upd({ name: e.target.value })}
                    />
                    {!isValidAgentName && (
                      <p className="text-sm text-red-500 mt-1">Only letters, numbers, hyphens and underscores allowed</p>
                    )}
                  </Field>
                  <Field label={t("agents.field_language")} className="w-full sm:w-36 sm:shrink-0">
                    <Select value={form.language} onValueChange={(v) => upd({ language: v })}>
                      <SelectTrigger className="w-full bg-background border-border text-sm h-8">
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
                  <Field label={t("agents.field_provider")}>
                    <Select
                      value={form.providerConnection || "__none__"}
                      onValueChange={(v) => {
                        if (v === "__none__") {
                          upd({ providerConnection: "", provider: "", model: "" });
                        } else {
                          const sp = llmProviders.find((p) => p.name === v);
                          upd({ providerConnection: v, provider: sp?.provider_type ?? "", model: sp?.default_model ?? "" });
                        }
                      }}
                    >
                      <SelectTrigger className="w-full bg-background border-border text-sm h-8">
                        <SelectValue placeholder={t("agents.field_provider")} />
                      </SelectTrigger>
                      <SelectContent className="border-border">
                        <SelectItem value="__none__" className="text-sm text-muted-foreground">
                          <span className="text-muted-foreground">&mdash;</span>
                        </SelectItem>
                        {llmProviders.map((conn) => (
                          <SelectItem key={conn.id} value={conn.name} className="text-sm font-mono">
                            <span className="flex items-center gap-2">
                              <Link2 className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                              <span>{conn.name}</span>
                              <span className="text-muted-foreground/60 text-[10px]">{conn.default_model}</span>
                            </span>
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </Field>
                  <Field label={t("agents.field_model")}>
                    {providerModelsLoading && (
                      <span className="text-xs text-muted-foreground animate-pulse mb-1">{t("agents.loading_models")}</span>
                    )}
                    {(() => {
                      const models = providerModels;
                      const isCustom = !models.includes(form.model);
                      if (models.length > 0) {
                        return (
                          <div className="space-y-1.5">
                            <div className="flex gap-2">
                              <Select
                                value={isCustom ? "__custom__" : form.model}
                                onValueChange={(v) => { upd({ model: v === "__custom__" ? "" : v }); }}
                              >
                                <SelectTrigger className="bg-background border-border font-mono text-sm h-8">
                                  <SelectValue placeholder={t("agents.model_placeholder")} />
                                </SelectTrigger>
                                <SelectContent className="border-border max-h-60">
                                  {models.map((m) => (
                                    <SelectItem key={m} value={m} className="font-mono text-sm">{m}</SelectItem>
                                  ))}
                                  <SelectItem value="__custom__" className="font-mono text-sm italic text-muted-foreground">{t("agents.model_custom")}</SelectItem>
                                </SelectContent>
                              </Select>
                              <Button variant="outline" size="icon" className="shrink-0 h-8 w-8" onClick={() => refetchModels()} disabled={providerModelsLoading}>
                                <RefreshCw className={`h-3.5 w-3.5 ${providerModelsLoading ? "animate-spin" : ""}`} />
                              </Button>
                            </div>
                            {isCustom && (
                              <Input value={form.model} placeholder="custom-model-name" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ model: e.target.value })} />
                            )}
                          </div>
                        );
                      }
                      return (
                        <div className="flex gap-2">
                          <Input value={form.model} placeholder="model-name" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ model: e.target.value })} />
                          {selectedProvider && (
                            <Button variant="outline" size="sm" className="shrink-0 h-8 text-xs" onClick={() => refetchModels()} disabled={providerModelsLoading}>
                              <RefreshCw className={`h-3.5 w-3.5 ${providerModelsLoading ? "animate-spin" : ""}`} />
                              <span className="ml-1">Discover</span>
                            </Button>
                          )}
                        </div>
                      );
                    })()}
                  </Field>
                  <Field label={t("agents.field_temperature")}>
                    <Input type="number" step="0.1" min="0" max="2" value={form.temperature} className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ temperature: e.target.value })} />
                  </Field>
                  <Field label={t("agents.field_max_tokens")}>
                    <Input type="number" step="256" min="256" max="65536" value={form.maxTokens} placeholder="Auto" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ maxTokens: e.target.value })} />
                  </Field>
                  <Field label={t("agents.field_fallback_provider")}>
                    <Select value={form.fallbackProvider || "__none__"} onValueChange={(v) => upd({ fallbackProvider: v === "__none__" ? "" : v })}>
                      <SelectTrigger className="w-full bg-background border-border text-sm h-8">
                        <SelectValue placeholder={t("agents.field_fallback_provider")} />
                      </SelectTrigger>
                      <SelectContent className="border-border">
                        <SelectItem value="__none__" className="text-sm text-muted-foreground"><span className="text-muted-foreground">&mdash;</span></SelectItem>
                        {llmProviders.map((conn) => (
                          <SelectItem key={conn.id} value={conn.name} className="text-sm font-mono">
                            <span className="flex items-center gap-2">
                              <Link2 className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                              <span>{conn.name}</span>
                              <span className="text-muted-foreground/60 text-[10px]">{conn.default_model}</span>
                            </span>
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </Field>
                  <Field label={t("agents.field_top_k_tools")}>
                    <Input type="number" step="1" min="1" max="50" value={form.maxToolsInContext} placeholder={t("agents.placeholder_all")} className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ maxToolsInContext: e.target.value })} />
                  </Field>
                  <Field label={t("agents.field_voice_profile")}>
                    <Select value={form.voice || "__default__"} onValueChange={(v) => upd({ voice: v === "__default__" ? "" : v })}>
                      <SelectTrigger className="w-full bg-background border-border text-sm h-8"><SelectValue /></SelectTrigger>
                      <SelectContent className="border-border">
                        <SelectItem value="__default__">{t("common.default")}</SelectItem>
                        {voices.map((v) => (<SelectItem key={v.id} value={v.id}>{v.name}</SelectItem>))}
                      </SelectContent>
                    </Select>
                  </Field>
                  <Field label={t("agents.field_daily_budget")}>
                    <Input type="number" step="10000" min="0" value={form.dailyBudgetTokens} className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ dailyBudgetTokens: e.target.value })} />
                  </Field>
                </div>
              </>
            )}

            {/* ── Tools tab ── */}
            {activeTab === "tools" && (
              <>
                <SwitchSection title={t("agents.section_tool_policy")} enabled={form.toolsEnabled} onToggle={(v) => upd({ toolsEnabled: v })}>
                  <div className="space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-xs font-medium text-muted-foreground">{t("agents.allow_all_tools")}</span>
                      <Switch checked={form.toolsAllowAll} onCheckedChange={(v) => upd({ toolsAllowAll: v })} className="data-[state=checked]:bg-primary" />
                    </div>
                    <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                      <Field label={t("agents.field_allowed")}>
                        <ToolMultiSelect tools={toolNames} selected={form.toolsAllow.split(",").map((s) => s.trim()).filter(Boolean)} onChange={(v) => upd({ toolsAllow: v.join(", ") })} placeholder={t("common.select_tools_placeholder")} />
                      </Field>
                      <Field label={t("agents.field_denied")}>
                        <ToolMultiSelect tools={toolNames} selected={form.toolsDeny.split(",").map((s) => s.trim()).filter(Boolean)} onChange={(v) => upd({ toolsDeny: v.join(", ") })} placeholder={t("common.select_tools_placeholder")} />
                      </Field>
                    </div>
                    <div className="border-t border-border/20 pt-2 mt-2">
                      <span className="text-xs font-medium text-muted-foreground mb-1.5 block">{t("agents.tool_groups")}</span>
                      <div className="grid grid-cols-2 gap-1.5">
                        {([
                          ["toolGroupGit", t("agents.tool_group_git")] as const,
                          ["toolGroupManagement", t("agents.tool_group_management")] as const,
                          ["toolGroupSkillEditing", t("agents.tool_group_skills")] as const,
                          ["toolGroupSessionTools", t("agents.tool_group_sessions")] as const,
                        ]).map(([key, label]) => (
                          <label key={key} className="flex items-center gap-2 text-xs cursor-pointer">
                            <input type="checkbox" checked={form[key] as boolean} onChange={(e) => upd({ [key]: e.target.checked })} className="rounded border-border" />
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
                      <Field label={t("agents.approval_categories")}>
                        <div className="flex flex-col gap-1.5">
                          {(["system", "destructive", "external"] as const).map((cat) => (
                            <label key={cat} className="flex items-center gap-2 text-xs cursor-pointer">
                              <input type="checkbox" checked={form.approvalCategories.includes(cat)} onChange={(e) => {
                                const next = e.target.checked ? [...form.approvalCategories, cat] : form.approvalCategories.filter((c: string) => c !== cat);
                                upd({ approvalCategories: next });
                              }} className="rounded border-border" />
                              <span className="font-mono">{cat}</span>
                              <span className="text-muted-foreground/60 text-[10px]">
                                {cat === "system" ? "(shell, code, git)" : cat === "destructive" ? "(write, delete, edit)" : "(all other tools)"}
                              </span>
                            </label>
                          ))}
                        </div>
                      </Field>
                      <Field label={t("agents.approval_timeout")}>
                        <Input type="number" min="30" max="3600" step="30" value={form.approvalTimeout} className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ approvalTimeout: e.target.value })} />
                        <span className="text-[10px] text-muted-foreground">{t("agents.approval_timeout_hint")}</span>
                      </Field>
                    </div>
                    <Field label={t("agents.approval_specific_tools")}>
                      <ToolMultiSelect tools={toolNames} selected={form.approvalRequireFor} onChange={(v) => upd({ approvalRequireFor: v })} placeholder={t("agents.approval_tools_placeholder")} />
                    </Field>
                  </div>
                </SwitchSection>
              </>
            )}

            {/* ── Behavior tab ── */}
            {activeTab === "behavior" && (
              <>
                <SwitchSection title={t("agents.section_tool_loop")} enabled={form.tlEnabled} onToggle={(v) => upd({ tlEnabled: v })}>
                  <div className="space-y-2">
                    <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
                      <Field label={t("agents.field_tl_max_iterations")}>
                        <Input type="number" step="1" min="0" max="10000" className="bg-background border-border font-mono text-sm h-8" value={form.tlMaxIterations} onChange={(e) => upd({ tlMaxIterations: e.target.value })} />
                        <span className="text-[10px] text-muted-foreground">{t("agents.hint_tl_max_iterations")}</span>
                      </Field>
                      <Field label={t("agents.field_tl_warn_threshold")}>
                        <Input type="number" step="1" min="1" className="bg-background border-border font-mono text-sm h-8" value={form.tlWarnThreshold} onChange={(e) => upd({ tlWarnThreshold: e.target.value })} />
                      </Field>
                      <Field label={t("agents.field_tl_break_threshold")}>
                        <Input type="number" step="1" min="1" className="bg-background border-border font-mono text-sm h-8" value={form.tlBreakThreshold} onChange={(e) => upd({ tlBreakThreshold: e.target.value })} />
                      </Field>
                    </div>
                    <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
                      <Field label={t("agents.field_max_auto_continues")}>
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
                <SwitchSection title={t("agents.section_hooks")} enabled={form.hooksLogAll || form.hooksBlockTools.trim() !== ""} onToggle={(v) => { if (!v) upd({ hooksLogAll: false, hooksBlockTools: "" }); else upd({ hooksLogAll: true }); }}>
                  <div className="space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-xs font-medium text-muted-foreground">{t("agents.hooks_log_all")}</span>
                      <Switch checked={form.hooksLogAll} onCheckedChange={(v) => upd({ hooksLogAll: v })} className="data-[state=checked]:bg-primary" />
                    </div>
                    <Field label={t("agents.hooks_block_tools")}>
                      <Input value={form.hooksBlockTools} placeholder="tool1, tool2" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ hooksBlockTools: e.target.value })} />
                    </Field>
                  </div>
                </SwitchSection>
              </>
            )}

            {/* ── Session tab ── */}
            {activeTab === "session" && (
              <>
                <SwitchSection title={t("agents.section_session")} enabled={form.sessionEnabled} onToggle={(v) => upd({ sessionEnabled: v })}>
                  <div className="space-y-2">
                    <Field label={t("agents.field_dm_scope")}>
                      <Select value={form.sessionDmScope} onValueChange={(v) => upd({ sessionDmScope: v })}>
                        <SelectTrigger className="w-full bg-background border-border text-sm h-8"><SelectValue /></SelectTrigger>
                        <SelectContent className="border-border">
                          <SelectItem value="per-channel-peer">{t("agents.dm_scope_per_channel_peer")}</SelectItem>
                          <SelectItem value="shared">{t("agents.dm_scope_shared")}</SelectItem>
                          <SelectItem value="per-peer">{t("agents.dm_scope_per_peer")}</SelectItem>
                          <SelectItem value="per-chat">{t("agents.dm_scope_per_chat")}</SelectItem>
                        </SelectContent>
                      </Select>
                    </Field>
                    <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                      <Field label={t("agents.field_ttl_days")}>
                        <Input type="number" step="1" min="0" className="bg-background border-border font-mono text-sm h-8" value={form.sessionTtlDays} onChange={(e) => upd({ sessionTtlDays: e.target.value })} />
                      </Field>
                      <Field label={t("agents.field_max_messages")}>
                        <Input type="number" step="1" min="0" className="bg-background border-border font-mono text-sm h-8" value={form.sessionMaxMessages} onChange={(e) => upd({ sessionMaxMessages: e.target.value })} />
                      </Field>
                      <Field label={t("agents.field_prune_tool_output")}>
                        <Input type="number" step="1" min="0" value={form.sessionPruneToolOutput} placeholder="Off" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ sessionPruneToolOutput: e.target.value })} />
                      </Field>
                      <Field label={t("agents.field_max_history")}>
                        <Input type="number" step="1" min="0" value={form.maxHistoryMessages} placeholder="Unlimited" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ maxHistoryMessages: e.target.value })} />
                      </Field>
                    </div>
                  </div>
                </SwitchSection>
                <SwitchSection title={t("agents.section_access")} enabled={form.accessEnabled} onToggle={(v) => upd({ accessEnabled: v })}>
                  <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                    <Field label={t("agents.field_access_mode")}>
                      <Select value={form.accessMode} onValueChange={(v) => upd({ accessMode: v })}>
                        <SelectTrigger className="bg-background border-border text-sm h-8"><SelectValue /></SelectTrigger>
                        <SelectContent>
                          <SelectItem value="open">Open</SelectItem>
                          <SelectItem value="restricted">Restricted</SelectItem>
                        </SelectContent>
                      </Select>
                    </Field>
                    {form.accessMode === "restricted" && (
                      <Field label={t("agents.field_access_owner_id")}>
                        <Input value={form.accessOwnerId} placeholder="Telegram User ID" className="bg-background border-border font-mono text-sm h-8" onChange={(e) => upd({ accessOwnerId: e.target.value })} />
                      </Field>
                    )}
                  </div>
                </SwitchSection>
              </>
            )}

            {/* ── Schedule tab ── */}
            {activeTab === "schedule" && (
              <SwitchSection title={t("agents.section_schedule")} enabled={form.hbEnabled} onToggle={(v) => upd({ hbEnabled: v })}>
                <CronSchedulePicker value={form.hbCron} onChange={(v) => upd({ hbCron: v })} timezone={form.hbTimezone || "UTC"} onTimezoneChange={(v) => upd({ hbTimezone: v })} />
                <Field label={t("agents.field_announce_to")}>
                  <Select value={form.hbAnnounceTo || "__none__"} onValueChange={(v) => upd({ hbAnnounceTo: v === "__none__" ? "" : v })}>
                    <SelectTrigger className="w-full bg-background border-border text-sm h-8"><SelectValue /></SelectTrigger>
                    <SelectContent className="border-border">
                      <SelectItem value="__none__">&mdash;</SelectItem>
                      <SelectItem value="telegram">Telegram</SelectItem>
                      <SelectItem value="discord">Discord</SelectItem>
                    </SelectContent>
                  </Select>
                </Field>
              </SwitchSection>
            )}

            {/* ── Channels tab ── */}
            {activeTab === "channels" && (
              <>
                <RoutingRulesEditor routing={form.routing} llmProviders={llmProviders} discoveredModels={discoveredModels} fetchModels={fetchModels} onChange={(routing) => upd({ routing })} />
                {editName && (
                  <div className="space-y-2 border-t border-border/30 pt-3">
                    <div className="flex items-center justify-between">
                      <h3 className="text-xs font-semibold uppercase tracking-wide text-foreground">{t("agents.section_channels")}</h3>
                      <a href="/channels/" className="inline-flex items-center gap-1 text-xs text-primary hover:text-primary/80 transition-colors">
                        {t("agents.manage_channels")}
                        <ExternalLink className="h-3 w-3" />
                      </a>
                    </div>
                    {channels.length === 0 ? (
                      <p className="text-xs text-muted-foreground/60 py-2">{t("agents.no_channels")}</p>
                    ) : (
                      <div className="space-y-2">
                        {channels.map((ch) => (
                          <div key={ch.id} className="flex items-center gap-3 rounded-lg border border-border bg-muted/20 px-3 py-2.5">
                            <div className={`h-2 w-2 rounded-full shrink-0 ${ch.status === "running" ? "bg-success" : ch.status === "error" ? "bg-destructive" : "bg-muted-foreground/40"}`} />
                            <div className="flex-1 min-w-0">
                              <p className="text-xs font-semibold truncate text-foreground">{ch.display_name}</p>
                              <p className="text-[10px] text-muted-foreground font-mono uppercase">{ch.channel_type}</p>
                            </div>
                            <span className="text-[9px] font-mono text-muted-foreground/40">{ch.id.slice(0, 8)}</span>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                )}
              </>
            )}

          </div>
        </ScrollArea>

        <DialogFooter className="px-5 py-3 border-t border-border bg-muted/20">
          <div className="flex gap-3 w-full justify-end">
            <Button variant="ghost" size="sm" className="text-muted-foreground hover:text-foreground" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button size="sm" onClick={onSave} disabled={saving || !canSave} className="px-5 font-semibold">
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
      <DialogContent className="border-border shadow-2xl max-w-[calc(100%-1rem)] sm:max-w-sm max-h-[90vh] overflow-y-auto rounded-xl p-0">
        <DialogHeader className="px-5 py-4 border-b border-border bg-muted/20">
          <DialogTitle className="text-sm font-bold">{channelDialogId ? t("agents.channel_edit") : t("agents.channel_add_dialog")}</DialogTitle>
        </DialogHeader>
        <div className="px-5 py-4 space-y-4">
          <Field label={t("agents.channel_field_type")}>
            <Select
              value={channelForm.channel_type}
              onValueChange={(v) => setChannelForm((f) => ({ ...f, channel_type: v }))}
              disabled={!!channelDialogId}
            >
              <SelectTrigger className="w-full bg-background border-border text-sm h-8">
                <SelectValue />
              </SelectTrigger>
              <SelectContent className="border-border">
                <SelectItem value="telegram">Telegram</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          <Field label={t("agents.channel_field_display_name")}>
            <Input
              value={channelForm.display_name}
              placeholder={t("agents.channel_placeholder_name")}
              className="bg-background border-border text-sm h-8"
              onChange={(e) => setChannelForm((f) => ({ ...f, display_name: e.target.value }))}
            />
          </Field>
          <Field label={t("agents.channel_field_bot_token")}>
            <Input
              type="password"
              value={channelForm.bot_token}
              placeholder="5092...:AAE..."
              className="bg-background border-border font-mono text-sm h-8"
              onChange={(e) => setChannelForm((f) => ({ ...f, bot_token: e.target.value }))}
            />
          </Field>
          <Field label={t("agents.channel_field_api_url")}>
            <Input
              value={channelForm.api_url}
              placeholder="http://localhost:8081"
              className="bg-background border-border font-mono text-sm h-8"
              onChange={(e) => setChannelForm((f) => ({ ...f, api_url: e.target.value }))}
            />
          </Field>
        </div>
        <DialogFooter className="px-5 py-3 border-t border-border bg-muted/20">
          <div className="flex gap-3 w-full justify-end">
            <Button variant="ghost" size="sm" className="text-muted-foreground" onClick={() => onOpenChange(false)}>{t("common.cancel")}</Button>
            <Button
              size="sm"
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
      <AlertDialogContent className="border-border shadow-2xl rounded-xl">
        <AlertDialogHeader>
          <AlertDialogTitle className="text-base font-bold text-destructive">{t("agents.delete_channel_title")}</AlertDialogTitle>
          <AlertDialogDescription className="text-sm text-muted-foreground mt-2">
            {t("agents.delete_channel_description")}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter className="mt-6">
          <AlertDialogCancel className="border-border hover:bg-muted">{t("common.cancel")}</AlertDialogCancel>
          <AlertDialogAction
            onClick={() => deleteChannelId && onConfirm(deleteChannelId)}
            className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
          >
            {t("common.delete")}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

// --- Helper components ---

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-2">
      <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">{title}</h3>
      {children}
    </div>
  );
}

function SwitchSection({
  title,
  enabled,
  onToggle,
  children,
}: {
  title: string;
  enabled: boolean;
  onToggle: (v: boolean) => void;
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
          className="data-[state=checked]:bg-primary"
        />
      </div>
      {enabled && <div className="animate-in fade-in duration-200">{children}</div>}
    </div>
  );
}

function Field({
  label,
  children,
  className,
}: {
  label: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={`flex flex-col gap-1.5 ${className ?? ""}`}>
      <label className="text-xs font-medium text-muted-foreground">{label}</label>
      {children}
    </div>
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
