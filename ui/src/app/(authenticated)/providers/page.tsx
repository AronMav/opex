"use client";

import { useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Textarea } from "@/components/ui/textarea";
import { toast } from "sonner";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
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
  Key,
  RefreshCw,
  Check,
  Globe,
  Plus,
  Pencil,
  Trash2,
  Link2,
  Zap,
  Mic,
  Volume2,
  Eye,
  Image as ImageIcon,
  Brain,
} from "lucide-react";
import type { Provider, CreateProviderInput, ProviderOptions } from "@/types/api";
import { apiGet, apiPost } from "@/lib/api";
import { TimeoutsSection } from "./_parts/TimeoutsSection";
import {
  useProviders,
  useProviderTypes,
  useProviderActive,
  useCreateProvider,
  useUpdateProvider,
  useDeleteProvider,
  useSetProviderActive,
  useMediaDrivers,
} from "@/lib/queries";

// ── Constants ────────────────────────────────────────────────────────────────

const ALL_CATEGORIES = ["text", "stt", "tts", "vision", "imagegen", "embedding"] as const;
type ProviderCategory = typeof ALL_CATEGORIES[number];

const ALL_CAPABILITIES = ["stt", "tts", "vision", "imagegen", "embedding"] as const;

/** Capabilities that require toolgate restart when active provider changes */



/** Category-specific badge colors — intentionally distinct per capability */
const CATEGORY_BADGE_CLASS: Record<ProviderCategory, string> = {
  text: "bg-amber-500/10 text-amber-600 border-amber-500/20",
  stt: "bg-blue-500/10 text-blue-500 border-blue-500/20",
  tts: "bg-green-500/10 text-green-500 border-green-500/20",
  vision: "bg-purple-500/10 text-purple-500 border-purple-500/20",
  imagegen: "bg-orange-500/10 text-orange-500 border-orange-500/20",
  embedding: "bg-cyan-500/10 text-cyan-500 border-cyan-500/20",
};

const CAPABILITY_BADGE_CLASS: Record<string, string> = {
  ...CATEGORY_BADGE_CLASS,
};

const CATEGORY_ICONS: Record<ProviderCategory, React.ReactNode> = {
  text: <Link2 className="h-3.5 w-3.5" />,
  stt: <Mic className="h-3.5 w-3.5" />,
  tts: <Volume2 className="h-3.5 w-3.5" />,
  vision: <Eye className="h-3.5 w-3.5" />,
  imagegen: <ImageIcon className="h-3.5 w-3.5" />,
  embedding: <Brain className="h-3.5 w-3.5" />,
};

const CAPABILITY_ICONS: Record<string, React.ReactNode> = {
  ...CATEGORY_ICONS,
};

const EMPTY_FORM: CreateProviderInput = {
  name: "",
  type: "",
  provider_type: "",
  base_url: "",
  default_model: "",
  notes: "",
  enabled: true,
};

type DialogState =
  | { open: false }
  | { open: true; category: ProviderCategory | ""; editing: Provider | null };

// ── Main page ────────────────────────────────────────────────────────────────

export default function ProvidersPage() {
  const { t } = useTranslation();

  const capLabel = (cap: string) => {
    const key: Record<string, string> = {
      text: "providers.cap_text", stt: "providers.cap_stt", tts: "providers.cap_tts",
      vision: "providers.cap_vision", imagegen: "providers.cap_imagegen",
      embedding: "providers.cap_embedding",
    };
    return key[cap] ? t(key[cap] as Parameters<typeof t>[0]) : cap;
  };

  // Unified hooks
  const { data: providers = [], isLoading, refetch } = useProviders();
  const { data: providerTypes = [] } = useProviderTypes();
  const { data: active = [] } = useProviderActive();
  const { data: driversMap = {} } = useMediaDrivers();
  const createProvider = useCreateProvider();
  const updateProvider = useUpdateProvider();
  const deleteProvider = useDeleteProvider();
  const setActive = useSetProviderActive();


  // Dialog state
  const [dialog, setDialog] = useState<DialogState>({ open: false });
  const [form, setForm] = useState<CreateProviderInput>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [apiKeyValue, setApiKeyValue] = useState("");
  const [discoveredModels, setDiscoveredModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [testResult, setTestResult] = useState<{
    cli_found?: boolean;
    cli_path?: string;
    cli_version?: string;
    auth_ok?: boolean;
    response_ok?: boolean;
    response_time_ms?: number;
    error?: string;
  } | null>(null);
  const [testLoading, setTestLoading] = useState(false);

  // Delete state
  const [deleteTarget, setDeleteTarget] = useState<Provider | null>(null);

  // ── Active helpers ────────────────────────────────────────────────────────

  const getActiveName = (capability: string) =>
    active.find((a) => a.capability === capability)?.provider_name ?? null;

  const providersForCapability = (cap: string) => {
    return providers.filter((p) => p.type === cap);
  };

  // ── LLM helpers ────────────────────────────────────────────────────────────

  const defaultUrlFor = (typeId: string) =>
    providerTypes.find((pt) => pt.id === typeId)?.default_base_url ?? "";

  const dialogCategory = dialog.open ? dialog.category : "";

  const selectedType = dialog.open && dialog.category === "text"
    ? providerTypes.find((pt) => pt.id === form.provider_type)
    : undefined;

  const isCli = dialog.open && form.provider_type.endsWith("-cli");

  const testConnection = async () => {
    if (!dialog.open || !dialog.editing) return;
    setTestLoading(true);
    setTestResult(null);
    try {
      const result = await apiPost<{
        cli_found?: boolean;
        cli_path?: string;
        cli_version?: string;
        auth_ok?: boolean;
        response_ok?: boolean;
        response_time_ms?: number;
        error?: string;
      }>(`/api/providers/${dialog.editing.id}/test-cli`, {});
      setTestResult(result);
    } catch (e) {
      setTestResult({ cli_found: false, error: String(e) });
    }
    setTestLoading(false);
  };

  const discoverModels = async () => {
    if (!form.provider_type) return;
    setModelsLoading(true);
    try {
      let url: string;
      if (dialog.open && dialog.editing) {
        url = `/api/providers/${dialog.editing.id}/models`;
      } else {
        const baseUrl = form.base_url || undefined;
        url = `/api/providers/${form.provider_type}/models${baseUrl ? `?base_url=${encodeURIComponent(baseUrl)}` : ""}`;
      }
      const data = await apiGet<{ models: { id: string }[] | string[] }>(url);
      setDiscoveredModels(data.models.map((m) => typeof m === "string" ? m : m.id));
    } catch (e) {
      console.warn("[providers] model discovery failed:", e);
      toast.warning(t("providers.discover_failed"));
    }
    setModelsLoading(false);
  };

  // ── Open create ────────────────────────────────────────────────────────────

  const openCreate = () => {
    setForm(EMPTY_FORM);
    setApiKeyValue("");
    setDiscoveredModels([]);
    setTestResult(null);
    setTestLoading(false);
    setDialog({ open: true, category: "", editing: null });
  };

  // ── Open edit ──────────────────────────────────────────────────────────────

  const openEdit = (p: Provider) => {
    setForm({
      name: p.name,
      type: p.type,
      provider_type: p.provider_type,
      base_url: p.base_url ?? "",
      default_model: p.default_model ?? "",
      notes: p.notes ?? "",
      enabled: p.enabled,
      options: p.options,
    });
    setApiKeyValue("");
    setDiscoveredModels([]);
    setTestResult(null);
    setTestLoading(false);
    setDialog({ open: true, category: p.type as ProviderCategory, editing: p });
  };

  // ── Save ───────────────────────────────────────────────────────────────────

  const save = async () => {
    if (!dialog.open || dialog.category === "") return;
    setSaving(true);
    try {
      const body: CreateProviderInput = {
        ...form,
        type: dialog.category,
        base_url: form.base_url || undefined,
        default_model: form.default_model || undefined,
        notes: form.notes || undefined,
      };
      if (apiKeyValue.trim()) {
        body.api_key = apiKeyValue.trim();
      }
      if (dialog.editing) {
        await updateProvider.mutateAsync({ id: dialog.editing.id, ...body });
      } else {
        await createProvider.mutateAsync(body);
      }
      setDialog({ open: false });
    } catch (e) {
      toast.error(t("providers.save_error", { error: String(e) }));
    }
    setSaving(false);
  };

  // ── Delete ─────────────────────────────────────────────────────────────────

  const confirmDelete = () => {
    if (!deleteTarget) return;
    const target = deleteTarget;
    setDeleteTarget(null);
    deleteProvider.mutate(target.id, {
      onError: (e: Error) => toast.error(t("providers.delete_error", { error: e.message })),
    });
  };

  // ── Category change in dialog ──────────────────────────────────────────────

  const setCategory = (cat: ProviderCategory) => {
    if (!dialog.open) return;
    setForm((f) => ({ ...f, type: cat, provider_type: "", default_model: "" }));
    setDiscoveredModels([]);
    setDialog({ ...dialog, category: cat });
  };

  // ── Form validation ────────────────────────────────────────────────────────

  const isEditing = dialog.open && dialog.editing !== null;

  const isFormValid = (): boolean => {
    if (!dialog.open || dialog.category === "") return false;
    if (form.name.trim().length === 0) return false;
    if (dialog.category === "text") {
      return (
        form.provider_type.length > 0 &&
        (selectedType?.requires_api_key === false || dialog.editing?.has_api_key || apiKeyValue.trim().length > 0) &&
        (form.default_model?.trim() ?? "").length > 0
      );
    }
    // Media types
    return form.provider_type.length > 0;
  };

  // Drivers for selected media type
  const availableDrivers = dialog.open && dialog.category !== "" && dialog.category !== "text"
    ? (driversMap[dialog.category] ?? [])
    : [];

  // ── Render ─────────────────────────────────────────────────────────────────

  return (
    <div className="flex flex-col gap-8 p-4 md:p-6 lg:p-8 selection:bg-primary/20">
      {/* Header */}
      <div className="flex flex-col md:flex-row md:items-center justify-between gap-4">
        <div>
          <h2 className="font-display text-lg font-bold tracking-tight">
            {t("providers.title")}
          </h2>
          <p className="text-sm text-muted-foreground mt-1">
            {t("providers.subtitle")}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => refetch()}
            className="gap-1.5"
          >
            <RefreshCw className="h-4 w-4" />
            {t("common.refresh")}
          </Button>
          <Button size="sm" onClick={openCreate} className="gap-1.5">
            <Plus className="h-4 w-4" />
            {t("providers.add")}
          </Button>
        </div>
      </div>

      {/* Active Providers */}
      {providers.length > 0 && (
        <div className="rounded-xl border border-border/60 bg-card/50 p-5 space-y-3">
          <p className="text-xs font-medium text-muted-foreground uppercase tracking-wide">{t("providers.active_providers")}</p>
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
            {ALL_CAPABILITIES.map((cap) => {
              const capProviders = providersForCapability(cap);
              if (capProviders.length === 0) return null;
              const activeName = getActiveName(cap);
              return (
                <div key={cap} className="flex items-center gap-2 min-w-0">
                  <span className={`inline-flex items-center justify-center gap-1 text-[10px] font-semibold px-1.5 py-0.5 rounded border min-w-[5.5rem] shrink-0 ${CAPABILITY_BADGE_CLASS[cap] ?? "bg-muted text-muted-foreground border-border"}`}>
                    {CAPABILITY_ICONS[cap]}
                    {capLabel(cap)}
                  </span>
                  <Select
                    value={activeName ?? "__none__"}
                    onValueChange={(v) => {
                      const name = v === "__none__" ? null : v;
                      setActive.mutate({ capability: cap, provider_name: name }, {
                        onSuccess: () => {
                          toast.success(t("providers.active_updated", { capability: capLabel(cap) }));
                        },
                      });
                    }}
                  >
                    <SelectTrigger className="h-7 text-xs font-mono flex-1 min-w-0 truncate">
                      <SelectValue placeholder={t("providers.none")} />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="__none__" className="text-xs text-muted-foreground">
                        {t("providers.none")}
                      </SelectItem>
                      {capProviders.map((p) => (
                        <SelectItem key={p.name} value={p.name} className="text-xs font-mono">
                          {p.name}{p.default_model ? ` (${p.default_model})` : ""}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Provider grid */}
      {isLoading ? (
        <div className="flex justify-center py-12">
          <RefreshCw className="h-5 w-5 animate-spin text-muted-foreground" />
        </div>
      ) : providers.length === 0 ? (
        <div className="rounded-xl border border-dashed border-border/60 p-8 text-center text-muted-foreground text-sm">
          {t("providers.empty")}
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
          {providers.map((provider) => {
            const cap = provider.type as ProviderCategory;
            const badgeClass = CATEGORY_BADGE_CLASS[cap] ?? "bg-muted text-muted-foreground border-border";
            const typeLabel = provider.type === "text"
              ? (providerTypes.find((pt) => pt.id === provider.provider_type)?.name ?? provider.provider_type)
              : provider.provider_type;
            const isActive = getActiveName(provider.type) === provider.name;

            return (
              <div
                key={provider.id}
                className="rounded-xl border border-border/60 bg-card/50 p-5 space-y-4"
              >
                {/* Header */}
                <div className="flex items-start gap-3">
                  <div className="flex items-center justify-center h-9 w-9 rounded-lg bg-muted/50 border border-border/60 text-muted-foreground shrink-0">
                    {CATEGORY_ICONS[cap] ?? <Link2 className="h-4 w-4" />}
                  </div>
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-1.5">
                      <p className="font-semibold text-sm font-mono truncate">
                        {provider.name}
                      </p>
                      {isActive && (
                        <span className="text-[9px] font-bold px-1 py-0 rounded bg-primary/10 text-primary border border-primary/20">
                          {t("providers.active_badge")}
                        </span>
                      )}
                    </div>
                    <div className="flex items-center gap-1.5 mt-0.5 flex-wrap">
                      <span className={`inline-flex items-center gap-1 text-[10px] font-semibold px-1.5 py-0 rounded border ${badgeClass}`}>
                        {capLabel(cap)}
                      </span>
                      <Badge
                        variant="secondary"
                        className="text-[10px] px-1.5 py-0 font-mono"
                      >
                        {typeLabel}
                      </Badge>
                      {provider.default_model && (
                        <span className="text-[11px] text-muted-foreground font-mono truncate">
                          {provider.default_model}
                        </span>
                      )}
                    </div>
                  </div>
                </div>

                {/* Base URL */}
                {provider.base_url && (
                  <div className="flex items-center gap-1.5 text-xs text-muted-foreground/60 font-mono truncate">
                    <Globe className="h-3 w-3 shrink-0" />
                    <span className="truncate">{provider.base_url}</span>
                  </div>
                )}

                {/* API key status */}
                <div className="flex items-center gap-1.5 text-xs text-muted-foreground/70">
                  <Key className="h-3 w-3 shrink-0" />
                  <span className="font-mono truncate">
                    {provider.api_key ?? (provider.has_api_key ? t("providers.api_key_configured") : t("providers.api_key_not_set"))}
                  </span>
                </div>

                {/* Notes */}
                {provider.notes && (
                  <p className="text-[11px] text-muted-foreground/60 truncate">
                    {provider.notes}
                  </p>
                )}

                {/* Enabled badge for non-text */}
                {provider.type !== "text" && (
                  <div>
                    <Badge
                      variant="secondary"
                      className={`text-[10px] px-1.5 py-0 ${provider.enabled ? "text-green-600" : "text-muted-foreground"}`}
                    >
                      {provider.enabled ? t("providers.status_enabled") : t("providers.status_disabled")}
                    </Badge>
                  </div>
                )}

                {/* Actions */}
                <div className="flex items-center gap-2 pt-1 border-t border-border/30">
                  <Button
                    variant="outline"
                    size="sm"
                    className="flex-1 h-7 text-xs"
                    onClick={() => openEdit(provider)}
                  >
                    <Pencil className="h-3 w-3" />
                    {t("common.edit")}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-7 text-xs text-destructive hover:text-destructive"
                    onClick={() => setDeleteTarget(provider)}
                    aria-label={t("common.delete")}
                  >
                    <Trash2 className="h-3 w-3" />
                  </Button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* ── Add / Edit Dialog ──────────────────────────────────────────────── */}
      <Dialog
        open={dialog.open}
        onOpenChange={(o) => { if (!o) setDialog({ open: false }); }}
      >
        <DialogContent className="max-w-[95vw] sm:max-w-md max-h-[90vh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              {dialogCategory !== "" ? CATEGORY_ICONS[dialogCategory] : <Plus className="h-4 w-4" />}
              {isEditing ? t("providers.edit_title") : t("providers.add_title")}
            </DialogTitle>
          </DialogHeader>
          <div className="space-y-4 py-2">
            {/* Category picker */}
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {t("providers.field_category")} <span className="text-destructive">*</span>
              </label>
              <Select
                value={dialogCategory}
                onValueChange={(v) => setCategory(v as ProviderCategory)}
                disabled={false}
              >
                <SelectTrigger className="text-sm">
                  <SelectValue placeholder={t("providers.select_category")} />
                </SelectTrigger>
                <SelectContent>
                  {ALL_CATEGORIES.map((cat) => (
                    <SelectItem key={cat} value={cat} className="text-sm">
                      <span className="flex items-center gap-1.5">
                        {CATEGORY_ICONS[cat]}
                        {capLabel(cat)}
                      </span>
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            {/* ── Text (LLM) fields ──────────────────────────────────────── */}
            {dialogCategory === "text" && (
              <>
                {/* Name */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_name")} <span className="text-destructive">*</span>
                  </label>
                  <Input
                    placeholder="my-openai"
                    value={form.name}
                    onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
                    className="font-mono text-sm"
                  />
                  <p className="text-[11px] text-muted-foreground/60">
                    {t("providers.field_name_hint")}
                  </p>
                </div>

                {/* Provider Type */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_type")} <span className="text-destructive">*</span>
                  </label>
                  <Select
                    value={form.provider_type}
                    onValueChange={(v) => {
                      const pt = providerTypes.find((p) => p.id === v);
                      setDiscoveredModels([]);
                      setForm((f) => ({
                        ...f,
                        provider_type: v,
                        base_url: f.base_url || pt?.default_base_url || "",
                      }));
                    }}
                  >
                    <SelectTrigger className="text-sm">
                      <SelectValue placeholder={t("providers.field_type_placeholder")} />
                    </SelectTrigger>
                    <SelectContent>
                      {providerTypes.map((pt) => (
                        <SelectItem key={pt.id} value={pt.id} className="text-sm">
                          {pt.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>

                {/* API Key */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_api_key")} {selectedType?.requires_api_key !== false
                      ? <span className="text-destructive">*</span>
                      : <span className="text-muted-foreground/50 font-normal">({t("providers.optional")})</span>}
                  </label>
                  <Input
                    type="password"
                    placeholder={isEditing && dialog.editing?.has_api_key ? t("providers.field_api_key_keep") : t("providers.field_api_key_placeholder")}
                    value={apiKeyValue}
                    onChange={(e) => setApiKeyValue(e.target.value)}
                    className="font-mono text-sm"
                  />
                  {isEditing && dialog.editing?.api_key && (
                    <p className="text-[11px] text-muted-foreground/60 font-mono">
                      {t("providers.field_api_key_current", { key: dialog.editing.api_key })}
                    </p>
                  )}
                </div>

                {/* Default Model */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_model")} <span className="text-destructive">*</span>
                  </label>
                  {discoveredModels.length > 0 ? (
                    <div className="flex gap-2">
                      <Select
                        value={form.default_model ?? ""}
                        onValueChange={(v) => setForm((f) => ({ ...f, default_model: v }))}
                      >
                        <SelectTrigger className="font-mono text-sm">
                          <SelectValue placeholder={t("providers.select_model")} />
                        </SelectTrigger>
                        <SelectContent>
                          {discoveredModels.map((m) => (
                            <SelectItem key={m} value={m} className="font-mono text-sm">
                              {m}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <Button
                        variant="outline"
                        size="icon"
                        className="shrink-0 h-9 w-9"
                        onClick={discoverModels}
                        disabled={modelsLoading}
                      >
                        <RefreshCw className={`h-3.5 w-3.5 ${modelsLoading ? "animate-spin" : ""}`} />
                      </Button>
                    </div>
                  ) : (
                    <div className="flex gap-2">
                      <Input
                        placeholder="MiniMax-Text-01"
                        value={form.default_model ?? ""}
                        onChange={(e) => setForm((f) => ({ ...f, default_model: e.target.value }))}
                        className="font-mono text-sm"
                      />
                      {selectedType?.supports_model_listing && form.provider_type && (
                        <Button
                          variant="outline"
                          size="sm"
                          className="shrink-0 h-9 text-xs"
                          onClick={discoverModels}
                          disabled={modelsLoading}
                        >
                          {modelsLoading ? (
                            <RefreshCw className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <Zap className="h-3.5 w-3.5" />
                          )}
                          {t("providers.discover")}
                        </Button>
                      )}
                    </div>
                  )}
                  {selectedType?.supports_model_listing === false && (
                    <p className="text-[11px] text-warning">
                      {t("providers.no_model_discovery")}
                    </p>
                  )}
                  {!isEditing && selectedType?.requires_api_key !== false && selectedType?.supports_model_listing && (
                    <p className="text-[11px] text-muted-foreground/60">
                      {t("providers.save_first_to_discover")}
                    </p>
                  )}
                </div>

                {/* Base URL — hidden for CLI providers */}
                {!isCli && (
                  <div className="space-y-1.5">
                    <label className="text-xs font-medium text-muted-foreground">
                      {t("providers.field_base_url")}{" "}
                      <span className="text-muted-foreground/50 font-normal">({t("providers.optional")})</span>
                    </label>
                    <Input
                      placeholder={form.provider_type ? defaultUrlFor(form.provider_type) || "https://..." : "https://..."}
                      value={form.base_url ?? ""}
                      onChange={(e) => setForm((f) => ({ ...f, base_url: e.target.value }))}
                      className="font-mono text-sm"
                    />
                    <p className="text-[11px] text-muted-foreground/60">
                      {t("providers.field_url_hint")}
                    </p>
                  </div>
                )}

                {/* Timeouts — hidden for CLI providers */}
                {!isCli && (
                  <TimeoutsSection
                    value={(form.options as ProviderOptions | undefined)?.timeouts ?? {}}
                    onChange={(timeouts) =>
                      setForm((f) => ({
                        ...f,
                        options: {
                          ...((f.options as ProviderOptions | undefined) ?? {}),
                          timeouts,
                        },
                      }))
                    }
                  />
                )}

                {/* Max retries — hidden for CLI providers */}
                {!isCli && (
                  <fieldset className="border rounded-md p-3 space-y-2">
                    <legend className="text-sm font-medium">
                      {t("providers.max_retries_section")}
                    </legend>
                    <label className="flex items-center justify-between gap-4">
                      <span className="text-sm">{t("providers.max_retries_label")}</span>
                      <div className="flex items-center gap-2">
                        <input
                          type="number"
                          aria-label={t("providers.max_retries_label")}
                          value={(form.options as ProviderOptions | undefined)?.max_retries ?? 3}
                          onChange={(e) => {
                            const v = Number(e.target.value);
                            setForm((f) => ({
                              ...f,
                              options: {
                                ...((f.options as ProviderOptions | undefined) ?? {}),
                                max_retries: v,
                              },
                            }));
                          }}
                          className="w-24 rounded border bg-background px-2 py-1 text-sm"
                          min={1}
                          max={10}
                        />
                        {(() => {
                          const v = (form.options as ProviderOptions | undefined)?.max_retries ?? 3;
                          return (v < 1 || v > 10) ? (
                            <span className="text-xs text-destructive">{t("providers.max_retries_error")}</span>
                          ) : null;
                        })()}
                      </div>
                    </label>
                  </fieldset>
                )}

                {/* Test Connection for CLI providers */}
                {isCli && isEditing && (
                  <div className="space-y-2">
                    <Button
                      variant="outline"
                      size="sm"
                      className="w-full gap-1.5"
                      onClick={testConnection}
                      disabled={testLoading}
                    >
                      {testLoading ? (
                        <RefreshCw className="h-3.5 w-3.5 animate-spin" />
                      ) : (
                        <Zap className="h-3.5 w-3.5" />
                      )}
                      {testLoading ? t("providers.test_connection_testing") : t("providers.test_connection")}
                    </Button>

                    {testResult && (
                      <div className={`rounded-lg border p-3 text-xs space-y-1 ${
                        testResult.response_ok
                          ? "bg-green-500/10 border-green-500/20 text-green-600 dark:text-green-400"
                          : "bg-destructive/10 border-destructive/20 text-destructive"
                      }`}>
                        {testResult.response_ok ? (
                          <>
                            <p className="font-medium">{t("providers.test_cli_success", { ms: String(testResult.response_time_ms ?? 0) })}</p>
                            {testResult.cli_version && (
                              <p className="font-mono text-[11px] opacity-70">
                                {testResult.cli_version} — {testResult.cli_path}
                              </p>
                            )}
                          </>
                        ) : (
                          <>
                            <p className="font-medium">
                              {!testResult.cli_found
                                ? t("providers.test_cli_not_found")
                                : testResult.auth_ok === false
                                  ? t("providers.test_cli_no_auth")
                                  : t("providers.test_cli_failed")}
                            </p>
                            {testResult.error && (
                              <p className="font-mono text-[11px] opacity-70">{testResult.error}</p>
                            )}
                          </>
                        )}
                      </div>
                    )}
                  </div>
                )}

                {/* Notes */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_notes")}{" "}
                    <span className="text-muted-foreground/50 font-normal">({t("providers.optional")})</span>
                  </label>
                  <Textarea
                    placeholder={t("providers.field_notes_placeholder")}
                    value={form.notes ?? ""}
                    onChange={(e) => setForm((f) => ({ ...f, notes: e.target.value }))}
                    className="text-sm resize-none h-16"
                  />
                </div>
              </>
            )}

            {/* ── Media fields (STT/TTS/Vision/ImageGen/Embedding) ─────── */}
            {dialogCategory !== "" && dialogCategory !== "text" && (
              <>
                {/* Name */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_name")} <span className="text-destructive">*</span>
                  </label>
                  <Input
                    placeholder="local-whisper"
                    value={form.name}
                    onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
                    className="font-mono text-sm"
                  />
                  <p className="text-[11px] text-muted-foreground/60">
                    {t("providers.media_name_hint")}
                  </p>
                </div>

                {/* Provider Type / Driver */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_driver")} <span className="text-destructive">*</span>
                  </label>
                  {availableDrivers.length > 0 ? (
                    <Select
                      value={form.provider_type}
                      onValueChange={(v) => setForm((f) => ({ ...f, provider_type: v }))}
                    >
                      <SelectTrigger className="text-sm font-mono">
                        <SelectValue placeholder={t("providers.select_driver")} />
                      </SelectTrigger>
                      <SelectContent>
                        {availableDrivers.map((d) => (
                          <SelectItem key={d.driver} value={d.driver} className="text-sm font-mono">
                            {d.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  ) : (
                    <Input
                      placeholder="openai-compatible"
                      value={form.provider_type}
                      onChange={(e) => setForm((f) => ({ ...f, provider_type: e.target.value }))}
                      className="font-mono text-sm"
                    />
                  )}
                </div>

                {/* Base URL */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_base_url")}{" "}
                    <span className="text-muted-foreground/50 font-normal">({t("providers.optional")})</span>
                  </label>
                  <Input
                    placeholder="http://192.168.1.132:8300/v1"
                    value={form.base_url ?? ""}
                    onChange={(e) => setForm((f) => ({ ...f, base_url: e.target.value }))}
                    className="font-mono text-sm"
                  />
                </div>

                {/* Model */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_model_short")}{" "}
                    <span className="text-muted-foreground/50 font-normal">({t("providers.optional")})</span>
                  </label>
                  <Input
                    placeholder="Systran/faster-whisper-large-v3"
                    value={form.default_model ?? ""}
                    onChange={(e) => setForm((f) => ({ ...f, default_model: e.target.value }))}
                    className="font-mono text-sm"
                  />
                </div>

                {/* API Key */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_api_key")}{" "}
                    <span className="text-muted-foreground/50 font-normal">({t("providers.optional")})</span>
                  </label>
                  <Input
                    type="password"
                    placeholder={isEditing ? t("providers.field_api_key_keep_existing") : t("providers.field_api_key_placeholder")}
                    value={apiKeyValue}
                    onChange={(e) => setApiKeyValue(e.target.value)}
                    className="font-mono text-sm"
                  />
                  {isEditing && dialog.editing?.api_key && (
                    <p className="text-[11px] text-muted-foreground/60 font-mono">
                      {t("providers.field_api_key_current", { key: dialog.editing.api_key })}
                    </p>
                  )}
                  <p className="text-[11px] text-muted-foreground/60">
                    {t("providers.field_api_key_vault_hint")}
                  </p>
                </div>

                {/* Notes */}
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t("providers.field_notes")}{" "}
                    <span className="text-muted-foreground/50 font-normal">({t("providers.optional")})</span>
                  </label>
                  <Textarea
                    placeholder={t("providers.field_notes_placeholder")}
                    value={form.notes ?? ""}
                    onChange={(e) => setForm((f) => ({ ...f, notes: e.target.value }))}
                    className="text-sm resize-none h-16"
                  />
                </div>

                {/* Enabled */}
                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    id="provider-enabled"
                    checked={form.enabled ?? true}
                    onChange={(e) => setForm((f) => ({ ...f, enabled: e.target.checked }))}
                    className="rounded border-border"
                  />
                  <label htmlFor="provider-enabled" className="text-xs font-medium text-muted-foreground cursor-pointer">
                    {t("providers.field_enabled")}
                  </label>
                </div>
              </>
            )}
          </div>
          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => setDialog({ open: false })}
            >
              {t("common.cancel")}
            </Button>
            <Button onClick={save} disabled={saving || !isFormValid()}>
              {saving ? (
                <RefreshCw className="h-4 w-4 animate-spin" />
              ) : (
                <Check className="h-4 w-4" />
              )}
              {isEditing ? t("common.save") : t("common.create")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* ── Delete Confirmation ────────────────────────────────────────────── */}
      <AlertDialog
        open={!!deleteTarget}
        onOpenChange={(o) => { if (!o) setDeleteTarget(null); }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {t("providers.delete_title")}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {t("providers.delete_description")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDelete}
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
