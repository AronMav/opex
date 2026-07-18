"use client";

import React from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Field } from "@/components/ui/field";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { RefreshCw, Zap } from "lucide-react";
import type { CreateProviderInput, Provider, ProviderType } from "@/types/api";
import { TimeoutsSection } from "./TimeoutsSection";
import { ProviderPresetPicker, type CatalogProvider } from "./ProviderPresetPicker";
import { getOpts } from "./helpers";
import { ModelCombobox } from "@/components/provider-fields";

interface TestResult {
  cli_found?: boolean;
  cli_path?: string;
  cli_version?: string;
  auth_ok?: boolean;
  response_ok?: boolean;
  response_time_ms?: number;
  error?: string;
}

interface TextFieldsProps {
  form: CreateProviderInput;
  setForm: React.Dispatch<React.SetStateAction<CreateProviderInput>>;
  apiKeyValue: string;
  setApiKeyValue: (s: string) => void;
  providerTypes: ProviderType[];
  selectedType?: ProviderType;
  isCli: boolean;
  isEditing: boolean;
  editing: Provider | null;
  onSetProviderType: (v: string) => void;
  testResult: TestResult | null;
  testLoading: boolean;
  onTestConnection: () => void;
  defaultUrlFor: (id: string) => string;
  typeId: string;
  modelId: string;
  activeTab: "general" | "advanced";
}

export function TextFields({
  form,
  setForm,
  apiKeyValue,
  setApiKeyValue,
  providerTypes,
  selectedType,
  isCli,
  isEditing,
  editing,
  onSetProviderType,
  testResult,
  testLoading,
  onTestConnection,
  defaultUrlFor,
  typeId,
  modelId,
  activeTab,
}: TextFieldsProps) {
  const { t } = useTranslation();

  // Model suggestions from the picked catalog preset — the create flow has no
  // saved provider id to discover from, but the catalog already ships a list.
  const [presetModels, setPresetModels] = React.useState<string[]>([]);

  // Apply a catalog preset (models.dev/…) onto the form — the way to add the
  // hundreds of providers OPEX doesn't ship natively. Most are OpenAI-compatible
  // → provider_type `openai_compat` + the catalog base_url; natively-supported
  // ids (openai/anthropic/google/…) use their own type.
  const applyPreset = (p: CatalogProvider) => {
    setPresetModels(p.models ?? []);
    setForm((f) => ({
      ...f,
      name: f.name?.trim() ? f.name : p.id,
      // Backend-computed native type (moonshotai→moonshot, …) or openai_compat.
      provider_type: p.provider_type || "openai_compat",
      base_url: p.api ?? f.base_url,
      default_model: f.default_model?.trim() ? f.default_model : (p.models[0] ?? ""),
    }));
  };

  return (
    <>
      {activeTab === "general" && (<>
      {/* Preset picker — only when adding a NEW provider */}
      {!isEditing && <ProviderPresetPicker onPick={applyPreset} />}

      {/* Name */}
      <Field label={t("providers.field_name") + " *"} labelClassName="text-xs" hint={t("providers.field_name_hint")}>
        <Input
          placeholder="my-openai"
          value={form.name}
          onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
          className="font-mono text-sm"
        />
      </Field>


      {/* API Key */}
      <Field
        label={
          t("providers.field_api_key") +
          (selectedType?.requires_api_key !== false
            ? " *"
            : ` (${t("providers.optional")})`)
        }
        labelClassName="text-xs"
      >
        <Input
          type="password"
          placeholder={isEditing && editing?.has_api_key ? t("providers.field_api_key_keep") : t("providers.field_api_key_placeholder")}
          value={apiKeyValue}
          onChange={(e) => setApiKeyValue(e.target.value)}
          className="font-mono text-sm"
        />
      </Field>
      {isEditing && editing?.api_key && (
        <p className="text-2xs text-muted-foreground-subtle font-mono">
          {t("providers.field_api_key_current", { key: editing.api_key })}
        </p>
      )}

      {/* Default Model */}
      <div className="space-y-1.5">
        <label htmlFor={modelId} className="text-xs font-medium text-muted-foreground">
          {t("providers.field_model")} <span className="text-destructive">*</span>
        </label>
        <ModelCombobox
          id={modelId}
          value={form.default_model ?? ""}
          onChange={(v) => setForm((f) => ({ ...f, default_model: v }))}
          providerId={isEditing ? editing?.id ?? null : null}
          staticOptions={!isEditing ? presetModels : undefined}
          placeholder="MiniMax-Text-01"
        />
        {selectedType?.supports_model_listing === false && (
          <p className="text-2xs text-warning">{t("providers.no_model_discovery")}</p>
        )}
      </div>

      {/* Base URL — hidden for CLI providers */}
      {!isCli && (
        <Field label={`${t("providers.field_base_url")} (${t("providers.optional")})`} labelClassName="text-xs" hint={t("providers.field_url_hint")}>
          <Input
            placeholder={form.provider_type ? defaultUrlFor(form.provider_type) || "https://..." : "https://..."}
            value={form.base_url ?? ""}
            onChange={(e) => setForm((f) => ({ ...f, base_url: e.target.value }))}
            className="font-mono text-sm"
          />
        </Field>
      )}
      </>)}

      {activeTab === "advanced" && (<>
      {/* Provider Type — auto-set from the preset / defaults to openai_compat;
          here for override only. */}
      <div className="space-y-1.5">
        <label htmlFor={typeId} className="text-xs font-medium text-muted-foreground">
          {t("providers.field_type")}
        </label>
        <Select value={form.provider_type} onValueChange={onSetProviderType}>
          <SelectTrigger id={typeId} className="text-sm">
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

      {/* Timeouts — hidden for CLI providers */}
      {!isCli && (
        <TimeoutsSection
          value={getOpts(form).timeouts ?? {}}
          onChange={(timeouts) =>
            setForm((f) => ({ ...f, options: { ...getOpts(f), timeouts } }))
          }
        />
      )}

      {/* Max retries — hidden for CLI providers */}
      {!isCli && (
        <fieldset className="neu-inset rounded-lg p-3 space-y-2">
          <legend className="text-sm font-medium">
            {t("providers.max_retries_section")}
          </legend>
          <label htmlFor="prov-max-retries" className="flex items-center justify-between gap-4">
            <span className="text-sm">{t("providers.max_retries_label")}</span>
            <div className="flex items-center gap-2">
              <Input
                id="prov-max-retries"
                type="number"
                aria-label={t("providers.max_retries_label")}
                value={getOpts(form).max_retries ?? 3}
                onChange={(e) => {
                  const v = Number(e.target.value);
                  setForm((f) => ({ ...f, options: { ...getOpts(f), max_retries: v } }));
                }}
                className="w-24 text-sm h-8"
                min={1}
                max={10}
              />
              {(() => {
                const v = getOpts(form).max_retries ?? 3;
                return (v < 1 || v > 10) ? (
                  <span className="text-xs text-destructive">{t("providers.max_retries_error")}</span>
                ) : null;
              })()}
            </div>
          </label>
        </fieldset>
      )}
      </>)}


      {/* Test Connection for CLI providers (Advanced tab) */}
      {isCli && isEditing && activeTab === "advanced" && (
        <div className="space-y-2">
          <Button variant="outline" size="sm" className="w-full gap-1.5" onClick={onTestConnection} disabled={testLoading}>
            {testLoading ? <RefreshCw className="h-3.5 w-3.5 animate-spin" /> : <Zap className="h-3.5 w-3.5" />}
            {testLoading ? t("providers.test_connection_testing") : t("providers.test_connection")}
          </Button>
          {testResult && (
            <div className={`rounded-lg border p-3 text-xs space-y-1 ${
              testResult.response_ok
                ? "bg-success/10 border-success/20 text-success"
                : "bg-destructive/10 border-destructive/20 text-destructive"
            }`}>
              {testResult.response_ok ? (
                <>
                  <p className="font-medium">{t("providers.test_cli_success", { ms: String(testResult.response_time_ms ?? 0) })}</p>
                  {testResult.cli_version && (
                    <p className="font-mono text-2xs opacity-70">
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
                  {testResult.error && <p className="font-mono text-2xs opacity-70">{testResult.error}</p>}
                </>
              )}
            </div>
          )}
        </div>
      )}

      {activeTab === "general" && (
        /* Notes */
        <Field label={`${t("providers.field_notes")} (${t("providers.optional")})`} labelClassName="text-xs">
          <Textarea
            placeholder={t("providers.field_notes_placeholder")}
            value={form.notes ?? ""}
            onChange={(e) => setForm((f) => ({ ...f, notes: e.target.value }))}
            className="text-sm resize-none h-16"
          />
        </Field>
      )}
    </>
  );
}