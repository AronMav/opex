"use client";

import React from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
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
import { Check, Plus, RefreshCw } from "lucide-react";
import type { CreateProviderInput, Provider, ProviderType, MediaDriverInfo } from "@/types/api";
import { ALL_CATEGORIES, CATEGORY_ICONS } from "./_parts/constants";
import type { ProviderCategory } from "./_parts/constants";
import { TextFields } from "./_parts/TextFields";
import { MediaFields } from "./_parts/MediaFields";

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

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="max-w-[95vw] sm:max-w-xl max-h-[90dvh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            {category !== "" ? CATEGORY_ICONS[category] : <Plus className="h-4 w-4" />}
            {isEditing ? t("providers.edit_title") : t("providers.add_title")}
          </DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-2">
          {/* Category picker */}
          <div className="space-y-1.5">
            <label htmlFor={catId} className="text-xs font-medium text-muted-foreground">
              {t("providers.field_category")} <span className="text-destructive">*</span>
            </label>
            <Select
              value={category}
              onValueChange={(v) => onSetCategory(v as ProviderCategory)}
              disabled={false}
            >
              <SelectTrigger id={catId} className="text-sm">
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
            />
          )}
        </div>
        <DialogFooter>
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