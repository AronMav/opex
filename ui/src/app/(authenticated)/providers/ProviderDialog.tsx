"use client";

import React from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { DialogTabs } from "@/components/ui/dialog-tabs";
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
import { Check, Plus, RefreshCw, Settings2, SlidersHorizontal } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { CreateProviderInput, Provider, ProviderType, MediaDriverInfo } from "@/types/api";
import type { TranslationKey } from "@/i18n/types";
import { ALL_CATEGORIES, CATEGORY_ICONS } from "./_parts/constants";
import type { ProviderCategory } from "./_parts/constants";
import { TextFields } from "./_parts/TextFields";
import { MediaFields } from "./_parts/MediaFields";

// Per-model context windows are resolved automatically from the model catalog
// (models.dev/…), so the former "Models" tab (manual context-window table) was
// removed; a config-level `context_windows` override remains as an escape hatch.
export type ProviderTab = "general" | "advanced";

const TAB_META: Record<ProviderTab, { labelKey: TranslationKey; icon: LucideIcon }> = {
  general: { labelKey: "providers.tab_general", icon: Settings2 },
  advanced: { labelKey: "providers.tab_advanced", icon: SlidersHorizontal },
};

interface TestResult {
  cli_found?: boolean;
  cli_path?: string;
  cli_version?: string;
  auth_ok?: boolean;
  response_ok?: boolean;
  response_time_ms?: number;
  error?: string;
}

interface TtsVoice {
  id: string;
  name: string;
  description?: string;
  language?: string;
}

interface ProviderDialogProps {
  open: boolean;
  editing: Provider | null;
  category: ProviderCategory | "";
  form: CreateProviderInput;
  setForm: React.Dispatch<React.SetStateAction<CreateProviderInput>>;
  apiKeyValue: string;
  setApiKeyValue: (s: string) => void;
  providerTypes: ProviderType[];
  selectedType?: ProviderType;
  isCli: boolean;
  driversMap: Record<string, MediaDriverInfo[]>;
  saving: boolean;
  isFormValid: boolean;
  capLabel: (cap: string) => string;
  onSave: () => void;
  onClose: () => void;
  onSetCategory: (c: ProviderCategory) => void;
  onSetProviderType: (v: string) => void;
  discoveredModels: string[];
  modelsLoading: boolean;
  onDiscoverModels: () => void;
  testResult: TestResult | null;
  testLoading: boolean;
  onTestConnection: () => void;
  defaultUrlFor: (id: string) => string;
  ttsVoices: TtsVoice[];
  ttsVoicesLoading: boolean;
}

export function ProviderDialog(props: ProviderDialogProps) {
  const { t } = useTranslation();
  const {
    open,
    editing,
    category,
    form,
    setForm,
    apiKeyValue,
    setApiKeyValue,
    providerTypes,
    selectedType,
    isCli,
    driversMap,
    saving,
    isFormValid,
    capLabel,
    onSave,
    onClose,
    onSetCategory,
    onSetProviderType,
    discoveredModels,
    modelsLoading,
    onDiscoverModels,
    testResult,
    testLoading,
    onTestConnection,
    defaultUrlFor,
    ttsVoices,
    ttsVoicesLoading,
  } = props;

  const isEditing = editing !== null;
  const catId = React.useId();
  const typeId = React.useId();
  const modelId = React.useId();
  const driverId = React.useId();
  const voiceId = React.useId();
  const mediaKeyId = React.useId();

  const availableDrivers = category !== "" && category !== "text"
    ? (driversMap[category] ?? [])
    : [];

  // Which tabs apply to the current category. Text (LLM) providers get a
  // dedicated per-model "Models" tab (context windows); CLI text + media
  // providers have nothing model-tabbable, so only General + Advanced.
  const tabs: ProviderTab[] = React.useMemo(() => {
    if (category === "") return [];
    return ["general", "advanced"];
  }, [category]);

  const [activeTab, setActiveTab] = React.useState<ProviderTab>("general");
  // Keep the active tab valid when the category (and thus the tab set) changes.
  React.useEffect(() => {
    if (tabs.length && !tabs.includes(activeTab)) setActiveTab("general");
  }, [tabs, activeTab]);

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="flex flex-col max-h-[90dvh] overflow-hidden p-0 border-border shadow-2xl max-w-[calc(100%-1rem)] sm:max-w-xl rounded-xl">
        <DialogHeader className="px-5 pt-4 pb-0 border-b-0 bg-muted/20 space-y-3">
          <DialogTitle className="flex items-center gap-2 text-sm font-bold text-foreground truncate">
            {category !== "" ? CATEGORY_ICONS[category] : <Plus className="h-4 w-4" />}
            <span className="truncate">{isEditing ? t("providers.edit_title") : t("providers.add_title")}</span>
          </DialogTitle>

          {/* Category picker — label + select on one line; always visible,
              drives which tabs/fields show */}
          <div className="flex items-center gap-3">
            <label htmlFor={catId} className="text-xs font-medium text-muted-foreground shrink-0">
              {t("providers.field_category")} <span className="text-destructive">*</span>
            </label>
            <Select value={category} onValueChange={(v) => onSetCategory(v as ProviderCategory)}>
              <SelectTrigger id={catId} className="text-sm flex-1 min-w-0">
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

          {/* Tab bar — icon-only on mobile, icon+label for the active tab */}
          {tabs.length > 1 && (
            <DialogTabs
              items={tabs.map((id) => ({ value: id, label: t(TAB_META[id].labelKey), icon: TAB_META[id].icon }))}
              value={activeTab}
              onChange={setActiveTab}
            />
          )}
        </DialogHeader>
        <div className="border-t border-border bg-muted/10" />

        <div className="flex-1 min-h-0 overflow-y-auto overscroll-contain">
          <div className="px-5 py-4 space-y-3 min-w-0">
            {category === "" && (
              <p className="text-sm text-muted-foreground">{t("providers.select_category")}</p>
            )}

            {/* ── Text (LLM) fields ──────────────────────────────────────── */}
            {category === "text" && (
              <TextFields
                form={form}
                setForm={setForm}
                apiKeyValue={apiKeyValue}
                setApiKeyValue={setApiKeyValue}
                providerTypes={providerTypes}
                selectedType={selectedType}
                isCli={isCli}
                isEditing={isEditing}
                editing={editing}
                discoveredModels={discoveredModels}
                modelsLoading={modelsLoading}
                onDiscoverModels={onDiscoverModels}
                onSetProviderType={onSetProviderType}
                testResult={testResult}
                testLoading={testLoading}
                onTestConnection={onTestConnection}
                defaultUrlFor={defaultUrlFor}
                typeId={typeId}
                modelId={modelId}
                activeTab={activeTab}
              />
            )}

            {/* ── Media fields (STT/TTS/Vision/ImageGen/Embedding/WebSearch) ─── */}
            {category !== "" && category !== "text" && (
              <MediaFields
                form={form}
                setForm={setForm}
                apiKeyValue={apiKeyValue}
                setApiKeyValue={setApiKeyValue}
                isEditing={isEditing}
                editing={editing}
                availableDrivers={availableDrivers}
                ttsVoices={ttsVoices}
                ttsVoicesLoading={ttsVoicesLoading}
                driverId={driverId}
                voiceId={voiceId}
                mediaKeyId={mediaKeyId}
                activeTab={activeTab}
              />
            )}
          </div>
        </div>

        <DialogFooter className="px-5 py-3 border-t border-border bg-muted/20">
          <Button variant="ghost" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button onClick={onSave} disabled={saving || !isFormValid}>
            {saving ? <RefreshCw className="h-4 w-4 animate-spin" /> : <Check className="h-4 w-4" />}
            {isEditing ? t("common.save") : t("common.create")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}