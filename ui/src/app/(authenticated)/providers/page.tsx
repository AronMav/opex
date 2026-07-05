"use client";

import React, { useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { PageHeader } from "@/components/ui/page-header";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Skeleton } from "@/components/ui/skeleton";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { ScrollableTabsList } from "@/components/ui/scrollable-tabs-list";
import { SectionHeader } from "@/components/ui/section-header";
import { Plus, RefreshCw, Zap } from "lucide-react";
import { toast } from "sonner";
import type { Provider, CreateProviderInput, ProviderOptions } from "@/types/api";
import { apiGet, apiPost } from "@/lib/api";
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

// ── Re-exports for tests ─────────────────────────────────────────────────────
export { sortActiveRows, splitProviders, renumberPriorities, buildProviderBody, getOpts } from "./_parts/helpers";
export { ALL_CATEGORIES, ALL_CAPABILITIES } from "./_parts/constants";
export type { ProviderCategory } from "./_parts/constants";

import { ALL_CATEGORIES, ALL_CAPABILITIES, CATEGORY_ICONS, EMPTY_FORM } from "./_parts/constants";
import type { ProviderCategory } from "./_parts/constants";
import { sortActiveRows, splitProviders, renumberPriorities, buildProviderBody } from "./_parts/helpers";
import { ProviderRow } from "./ProviderRow";
import { ProviderSortableGroup } from "./ProviderSortableGroup";
import { ProviderDialog } from "./ProviderDialog";

// ── Main page ────────────────────────────────────────────────────────────────

type DialogState =
  | { open: false }
  | { open: true; category: ProviderCategory | ""; editing: Provider | null };

const CAP_DESC_KEY: Record<ProviderCategory, string> = {
  text: "providers.cap_text_desc",
  stt: "providers.cap_stt_desc",
  tts: "providers.cap_tts_desc",
  vision: "providers.cap_vision_desc",
  imagegen: "providers.cap_imagegen_desc",
  embedding: "providers.cap_embedding_desc",
  websearch: "providers.cap_websearch_desc",
};

export default function ProvidersPage() {
  const { t } = useTranslation();

  const capLabel = (cap: string) => {
    const key: Record<string, string> = {
      text: "providers.cap_text", stt: "providers.cap_stt", tts: "providers.cap_tts",
      vision: "providers.cap_vision", imagegen: "providers.cap_imagegen",
      embedding: "providers.cap_embedding", websearch: "providers.cap_websearch",
    };
    return key[cap] ? t(key[cap] as Parameters<typeof t>[0]) : cap;
  };

  // Unified hooks
  const { data: providers = [], isLoading, error, refetch } = useProviders();
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
  const [ttsVoices, setTtsVoices] = useState<{ id: string; name: string; description?: string; language?: string }[]>([]);
  const [ttsVoicesLoading, setTtsVoicesLoading] = useState(false);
  const [testResult, setTestResult] = useState<{
    cli_found?: boolean; cli_path?: string; cli_version?: string; auth_ok?: boolean;
    response_ok?: boolean; response_time_ms?: number; error?: string;
  } | null>(null);
  const [testLoading, setTestLoading] = useState(false);

  // Delete state
  const [deleteTarget, setDeleteTarget] = useState<Provider | null>(null);

  // ── Active helpers ────────────────────────────────────────────────────────

  const providersForCapability = (cap: string) => providers.filter((p) => p.type === cap);

  const setCapabilityActive = (
    capability: string,
    next: { provider_name: string; priority: number }[],
  ) => {
    setActive.mutate(
      { capability, providers: next },
      {
        onSuccess: () => toast.success(t("providers.active_updated", { capability: capLabel(capability) })),
        onError: (e: Error) => toast.error(t("providers.set_active_error", { error: e.message })),
      },
    );
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
        cli_found?: boolean; cli_path?: string; cli_version?: string; auth_ok?: boolean;
        response_ok?: boolean; response_time_ms?: number; error?: string;
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
    } catch {
      toast.warning(t("providers.discover_failed"));
    }
    setModelsLoading(false);
  };

  // ── TTS voice list loader ──────────────────────────────────────────────────
  React.useEffect(() => {
    if (!dialog.open || dialogCategory !== "tts" || !form.name) {
      setTtsVoices([]);
      return;
    }
    let cancelled = false;
    (async () => {
      setTtsVoicesLoading(true);
      try {
        const data = await apiGet<{ voices: { id: string; name: string; description?: string; language?: string }[] }>(
          `/api/tts/voices?provider=${encodeURIComponent(form.name)}`,
        );
        if (!cancelled) setTtsVoices(data.voices ?? []);
      } catch {
        if (!cancelled) setTtsVoices([]);
      } finally {
        if (!cancelled) setTtsVoicesLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, [dialog.open, dialogCategory, form.name]);

  // ── Open create ────────────────────────────────────────────────────────────

  const openCreate = () => {
    setForm(EMPTY_FORM);
    setApiKeyValue("");
    setDiscoveredModels([]);
    setTestResult(null);
    setTestLoading(false);
    setDialog({ open: true, category: "", editing: null });
  };

  const openEdit = (p: Provider) => {
    setForm({
      name: p.name, type: p.type, provider_type: p.provider_type,
      base_url: p.base_url ?? "", default_model: p.default_model ?? "",
      notes: p.notes ?? "", enabled: p.enabled, options: p.options,
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
      const body = buildProviderBody(form, apiKeyValue, dialog.category);
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
    // Text providers default to `openai_compat` so the user doesn't have to pick
    // a type up front — a catalog preset (or the Advanced tab) overrides it.
    setForm((f) => ({
      ...f,
      type: cat,
      provider_type: cat === "text" ? "openai_compat" : "",
      default_model: "",
    }));
    setDiscoveredModels([]);
    setDialog({ ...dialog, category: cat });
  };

  const onSetProviderType = (v: string) => {
    const pt = providerTypes.find((p) => p.id === v);
    setDiscoveredModels([]);
    setForm((f) => ({
      ...f,
      provider_type: v,
      base_url: f.base_url || pt?.default_base_url || "",
    }));
  };

  // ── Form validation ────────────────────────────────────────────────────────

  const isFormValid = (): boolean => {
    if (!dialog.open || dialog.category === "") return false;
    if (form.name.trim().length === 0) return false;
    if (dialog.category === "text") {
      const opts = (form.options as ProviderOptions | undefined);
      const mr = opts?.max_retries ?? 3;
      if (mr < 1 || mr > 10) return false;
      return (
        form.provider_type.length > 0 &&
        (selectedType?.requires_api_key === false || dialog.editing?.has_api_key || apiKeyValue.trim().length > 0) &&
        (form.default_model?.trim() ?? "").length > 0
      );
    }
    return form.provider_type.length > 0;
  };

  // ── Render ─────────────────────────────────────────────────────────────────

  return (
    <div className="flex flex-col gap-8 p-4 md:p-6 lg:p-8 min-w-0 selection:bg-primary/20">
      {/* Header */}
      <PageHeader
        title={t("providers.title")}
        description={t("providers.subtitle")}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => refetch()} className="gap-1.5">
              <RefreshCw className="h-4 w-4" />
              {t("common.refresh")}
            </Button>
            <Button size="lg" onClick={openCreate} className="w-full md:w-auto gap-2">
              <Plus className="h-4 w-4" />
              {t("providers.add")}
            </Button>
          </div>
        }
      />

      {error && <ErrorBanner error={String(error)} />}

      {/* Per-capability provider groups */}
      {isLoading ? (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4 gap-4">
          {[1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-48 rounded-xl" />
          ))}
        </div>
      ) : providers.length === 0 ? (
        <EmptyState icon={Zap} text={t("providers.empty")} height="h-48" />
      ) : (() => {
        const visibleCategories = ALL_CATEGORIES.filter((cap) => providersForCapability(cap).length > 0);
        if (visibleCategories.length === 0) {
          return <EmptyState icon={Zap} text={t("providers.empty")} height="h-48" />;
        }
        return (
          <Tabs defaultValue={visibleCategories[0]} className="min-w-0">
            <ScrollableTabsList className="h-9">
              {visibleCategories.map((cap) => (
                <TabsTrigger key={cap} value={cap}>
                  {CATEGORY_ICONS[cap]}
                  {capLabel(cap)}
                  <Badge variant="secondary" size="sm" className="ml-1.5">{providersForCapability(cap).length}</Badge>
                </TabsTrigger>
              ))}
            </ScrollableTabsList>

            {visibleCategories.map((cap) => {
              const capProviders = providersForCapability(cap);
              const activeRows = sortActiveRows(active, cap);
              const { active: activeList, inactive: inactiveList } = splitProviders(capProviders, activeRows);
              const isCapabilityGroup = (ALL_CAPABILITIES as readonly string[]).includes(cap);

              const typeLabelFor = (p: Provider) =>
                cap === "text"
                  ? (providerTypes.find((pt) => pt.id === p.provider_type)?.name ?? p.provider_type)
                  : p.provider_type;

              const activeNames = activeList.map((p) => p.name);
              const reorder = (orderedNames: string[]) =>
                setCapabilityActive(cap, renumberPriorities(orderedNames));
              const toggleOff = (p: Provider) =>
                setCapabilityActive(cap, renumberPriorities(activeNames.filter((n) => n !== p.name)));
              const toggleOn = (p: Provider) =>
                setCapabilityActive(cap, renumberPriorities([...activeNames, p.name]));

              return (
                <TabsContent key={cap} value={cap} className="mt-6">
                  {/* Capability description + active-routing hint */}
                  <p className="text-xs text-muted-foreground mb-4">
                    {t(CAP_DESC_KEY[cap] as Parameters<typeof t>[0])}
                    {isCapabilityGroup && cap !== "websearch" && (
                      <span className="text-muted-foreground-subtle"> · {t("providers.group_hint")}</span>
                    )}
                  </p>

                  {isCapabilityGroup ? (
                    <div className="space-y-6">
                      {/* Active — draggable */}
                      <div>
                        <SectionHeader
                          title={t("providers.active_heading")}
                          description={activeList.length > 1 ? t("providers.drag_hint") : undefined}
                        />
                        {activeList.length === 0 ? (
                          <p className="text-xs text-muted-foreground-subtle italic">{t("providers.none")}</p>
                        ) : (
                          <ProviderSortableGroup
                            cap={cap}
                            activeProviders={activeList}
                            typeLabelFor={typeLabelFor}
                            onReorder={reorder}
                            onToggleActive={toggleOff}
                            onEdit={openEdit}
                            onDelete={setDeleteTarget}
                          />
                        )}
                      </div>

                      {/* Inactive */}
                      {inactiveList.length > 0 && (
                        <div>
                          <SectionHeader title={t("providers.inactive_heading")} />
                          <div className="space-y-2">
                            {inactiveList.map((p) => (
                              <ProviderRow
                                key={p.id}
                                provider={p}
                                cap={cap}
                                isActive={false}
                                typeLabel={typeLabelFor(p)}
                                isCapabilityGroup
                                onToggleActive={() => toggleOn(p)}
                                onEdit={() => openEdit(p)}
                                onDelete={() => setDeleteTarget(p)}
                              />
                            ))}
                          </div>
                        </div>
                      )}
                    </div>
                  ) : (
                    /* text — plain rows, no priority */
                    <div className="space-y-2">
                      {capProviders
                        .slice()
                        .sort((a, b) => a.name.localeCompare(b.name))
                        .map((p) => (
                          <ProviderRow
                            key={p.id}
                            provider={p}
                            cap={cap}
                            isActive={false}
                            typeLabel={typeLabelFor(p)}
                            isCapabilityGroup={false}
                            onToggleActive={() => {}}
                            onEdit={() => openEdit(p)}
                            onDelete={() => setDeleteTarget(p)}
                          />
                        ))}
                    </div>
                  )}
                </TabsContent>
              );
            })}
          </Tabs>
        );
      })()}

      {/* ── Add / Edit Dialog ──────────────────────────────────────────────── */}
      <ProviderDialog
        open={dialog.open}
        editing={dialog.open ? dialog.editing : null}
        category={dialogCategory}
        form={form}
        setForm={setForm}
        apiKeyValue={apiKeyValue}
        setApiKeyValue={setApiKeyValue}
        providerTypes={providerTypes}
        selectedType={selectedType}
        isCli={isCli}
        driversMap={driversMap}
        saving={saving}
        isFormValid={isFormValid()}
        capLabel={capLabel}
        onSave={save}
        onClose={() => setDialog({ open: false })}
        onSetCategory={setCategory}
        onSetProviderType={onSetProviderType}
        discoveredModels={discoveredModels}
        modelsLoading={modelsLoading}
        onDiscoverModels={discoverModels}
        testResult={testResult}
        testLoading={testLoading}
        onTestConnection={testConnection}
        defaultUrlFor={defaultUrlFor}
        ttsVoices={ttsVoices}
        ttsVoicesLoading={ttsVoicesLoading}
      />

      {/* ── Delete Confirmation ────────────────────────────────────────────── */}
      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={confirmDelete}
        title={t("providers.delete_title")}
        description={t("providers.delete_description")}
      />
    </div>
  );
}