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
import { getOpts } from "./helpers";

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
  discoveredModels: string[];
  modelsLoading: boolean;
  onDiscoverModels: () => void;
  onSetProviderType: (v: string) => void;
  testResult: TestResult | null;
  testLoading: boolean;
  onTestConnection: () => void;
  defaultUrlFor: (id: string) => string;
  typeId: string;
  modelId: string;
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
  discoveredModels,
  modelsLoading,
  onDiscoverModels,
  onSetProviderType,
  testResult,
  testLoading,
  onTestConnection,
  defaultUrlFor,
  typeId,
  modelId,
}: TextFieldsProps) {
  const { t } = useTranslation();

  return (
    <>
      {/* Name */}
      <Field label={t("providers.field_name") + " *"} labelClassName="text-xs" hint={t("providers.field_name_hint")}>
        <Input
          placeholder="my-openai"
          value={form.name}
          onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
          className="font-mono text-sm"
        />
      </Field>

      {/* Provider Type */}
      <div className="space-y-1.5">
        <label htmlFor={typeId} className="text-xs font-medium text-muted-foreground">
          {t("providers.field_type")} <span className="text-destructive">*</span>
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
        {discoveredModels.length > 0 ? (
          <div className="flex gap-2">
            <Select
              value={form.default_model ?? ""}
              onValueChange={(v) => setForm((f) => ({ ...f, default_model: v }))}
            >
              <SelectTrigger id={modelId} className="font-mono text-sm">
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
            <Button variant="outline" size="icon" className="shrink-0 h-9 w-9" onClick={onDiscoverModels} disabled={modelsLoading} aria-label={t("providers.discover")} title={t("providers.discover")}>
              <RefreshCw className={`h-3.5 w-3.5 ${modelsLoading ? "animate-spin" : ""}`} />
            </Button>
          </div>
        ) : (
          <div className="flex gap-2">
            <Input
              id={modelId}
              placeholder="MiniMax-Text-01"
              value={form.default_model ?? ""}
              onChange={(e) => setForm((f) => ({ ...f, default_model: e.target.value }))}
              className="font-mono text-sm"
            />
            {selectedType?.supports_model_listing && form.provider_type && (
              <Button variant="outline" size="icon" className="shrink-0 h-9 w-9" onClick={onDiscoverModels} disabled={modelsLoading} aria-label={t("providers.discover")} title={t("providers.discover")}>
                <RefreshCw className={`h-3.5 w-3.5 ${modelsLoading ? "animate-spin" : ""}`} />
              </Button>
            )}
          </div>
        )}
        {selectedType?.supports_model_listing === false && (
          <p className="text-2xs text-warning">{t("providers.no_model_discovery")}</p>
        )}
        {!isEditing && selectedType?.requires_api_key !== false && selectedType?.supports_model_listing && (
          <p className="text-2xs text-muted-foreground-subtle">{t("providers.save_first_to_discover")}</p>
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

      {/* Per-model context window overrides — hidden for CLI providers. Each
          row: a model + tokens input. Empty = auto-detect via the provider API
          (/api/show, /v1/models), then name heuristic. Set explicitly only for
          models whose API doesn't report the window (e.g. MiMo). */}
      {!isCli && (() => {
        const cw = (getOpts(form).context_windows ?? {}) as Record<string, number>;
        const rows = Array.from(
          new Set([
            ...(form.default_model ? [form.default_model] : []),
            ...discoveredModels,
            ...Object.keys(cw),
          ]),
        ).filter(Boolean);
        const setCw = (model: string, raw: string) => {
          const v = raw.trim() === "" ? undefined : Number(raw);
          setForm((f) => {
            const opts = { ...getOpts(f) };
            const map: Record<string, number> = { ...((opts.context_windows as Record<string, number>) ?? {}) };
            if (v === undefined || Number.isNaN(v)) delete map[model];
            else map[model] = v;
            opts.context_windows = Object.keys(map).length ? map : undefined;
            return { ...f, options: opts };
          });
        };
        return (
          <fieldset className="neu-inset rounded-lg p-3 space-y-2">
            <legend className="text-sm font-medium">{t("providers.context_window_section")}</legend>
            <div className="flex items-center justify-between gap-3">
              <p className="text-xs text-muted-foreground">{t("providers.context_window_hint")}</p>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={onDiscoverModels}
                disabled={modelsLoading}
                className="shrink-0 h-8"
              >
                <RefreshCw className={`h-3.5 w-3.5 ${modelsLoading ? "animate-spin" : ""}`} />
                {t("providers.discover")}
              </Button>
            </div>
            {rows.length === 0 ? (
              <p className="text-2xs text-muted-foreground-subtle">{t("providers.context_window_no_models")}</p>
            ) : (
              <div className="max-h-56 overflow-y-auto divide-y divide-border/50">
                {rows.map((model, i) => {
                  const v = cw[model];
                  const invalid = typeof v === "number" && v > 0 && v < 1000;
                  const fid = `cw-${i}`;
                  return (
                    <label key={model} htmlFor={fid} className="flex items-center justify-between gap-3 py-1.5">
                      <span className="text-sm truncate" title={model}>{model}</span>
                      <div className="flex items-center gap-2 shrink-0">
                        <Input
                          id={fid}
                          type="number"
                          aria-label={`${model} ${t("providers.context_window_label")}`}
                          placeholder={t("providers.context_window_auto")}
                          value={cw[model] ?? ""}
                          onChange={(e) => setCw(model, e.target.value)}
                          className="w-32 text-sm h-8"
                          min={1000}
                          step={1000}
                        />
                        {invalid && (
                          <span className="text-xs text-destructive">{t("providers.context_window_error")}</span>
                        )}
                      </div>
                    </label>
                  );
                })}
              </div>
            )}
          </fieldset>
        );
      })()}

      {/* Test Connection for CLI providers */}
      {isCli && isEditing && (
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

      {/* Notes */}
      <Field label={`${t("providers.field_notes")} (${t("providers.optional")})`} labelClassName="text-xs">
        <Textarea
          placeholder={t("providers.field_notes_placeholder")}
          value={form.notes ?? ""}
          onChange={(e) => setForm((f) => ({ ...f, notes: e.target.value }))}
          className="text-sm resize-none h-16"
        />
      </Field>
    </>
  );
}