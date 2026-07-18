"use client";

import React from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { Field } from "@/components/ui/field";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { CreateProviderInput, Provider, MediaDriverInfo } from "@/types/api";
import { TimeoutsSection } from "./TimeoutsSection";
import { getOpts } from "./helpers";
import { ModelCombobox, VoiceSelect } from "@/components/provider-fields";

interface MediaFieldsProps {
  form: CreateProviderInput;
  setForm: React.Dispatch<React.SetStateAction<CreateProviderInput>>;
  apiKeyValue: string;
  setApiKeyValue: (s: string) => void;
  isEditing: boolean;
  editing: Provider | null;
  availableDrivers: MediaDriverInfo[];
  driverId: string;
  voiceId: string;
  mediaKeyId: string;
  activeTab: "general" | "advanced";
}

export function MediaFields({
  form,
  setForm,
  apiKeyValue,
  setApiKeyValue,
  isEditing,
  editing,
  availableDrivers,
  driverId,
  voiceId,
  mediaKeyId,
  activeTab,
}: MediaFieldsProps) {
  const { t } = useTranslation();
  const dialogCategory = form.type;
  const mediaModelIdLabel = React.useId();

  return (
    <>
      {activeTab === "general" && (<>
      {/* Name */}
      <Field label={t("providers.field_name") + " *"} labelClassName="text-xs" hint={t("providers.media_name_hint")}>
        <Input
          placeholder="local-whisper"
          value={form.name}
          onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
          className="font-mono text-sm"
        />
      </Field>

      {/* Provider Type / Driver */}
      <div className="space-y-1.5">
        <label htmlFor={driverId} className="text-xs font-medium text-muted-foreground">
          {t("providers.field_driver")} <span className="text-destructive">*</span>
        </label>
        {availableDrivers.length > 0 ? (
          <Select
            value={form.provider_type}
            onValueChange={(v) => setForm((f) => ({ ...f, provider_type: v }))}
          >
            <SelectTrigger id={driverId} className="text-sm font-mono">
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
            id={driverId}
            placeholder="openai-compatible"
            value={form.provider_type}
            onChange={(e) => setForm((f) => ({ ...f, provider_type: e.target.value }))}
            className="font-mono text-sm"
          />
        )}
      </div>

      {/* Base URL */}
      <Field label={`${t("providers.field_base_url")} (${t("providers.optional")})`} labelClassName="text-xs">
        <Input
          placeholder="http://192.168.1.132:8300/v1"
          value={form.base_url ?? ""}
          onChange={(e) => setForm((f) => ({ ...f, base_url: e.target.value }))}
          className="font-mono text-sm"
        />
      </Field>

      {/* Model */}
      <div className="space-y-1.5">
        <label htmlFor={mediaModelIdLabel} className="text-xs font-medium text-muted-foreground">
          {t("providers.field_model_short")}{" "}
          <span className="text-muted-foreground-subtle font-normal">({t("providers.optional")})</span>
        </label>
        <ModelCombobox
          id={mediaModelIdLabel}
          value={form.default_model ?? ""}
          onChange={(v) => setForm((f) => ({ ...f, default_model: v }))}
          providerId={isEditing ? editing?.id ?? null : null}
          placeholder="Systran/faster-whisper-large-v3"
        />
      </div>

      {/* Voice (TTS only) */}
      {dialogCategory === "tts" && (
        <div className="space-y-1.5">
          <label htmlFor={voiceId} className="text-xs font-medium text-muted-foreground">
            {t("providers.field_voice")}{" "}
            <span className="text-muted-foreground-subtle font-normal">({t("providers.optional")})</span>
          </label>
          <VoiceSelect
            id={voiceId}
            value={(getOpts(form).voice as string | undefined) ?? ""}
            onChange={(v) =>
              setForm((f) => ({ ...f, options: { ...getOpts(f), voice: v || undefined } }))
            }
            providerName={form.name}
            allowServerDefault
            className="text-sm font-mono"
          />
          <p className="text-2xs text-muted-foreground-subtle">{t("providers.field_voice_hint")}</p>
        </div>
      )}

      {/* API Key */}
      <div className="space-y-1.5">
        <label htmlFor={mediaKeyId} className="text-xs font-medium text-muted-foreground">
          {t("providers.field_api_key")}{" "}
          <span className="text-muted-foreground-subtle font-normal">({t("providers.optional")})</span>
        </label>
        <Input
          id={mediaKeyId}
          type="password"
          placeholder={isEditing ? t("providers.field_api_key_keep_existing") : t("providers.field_api_key_placeholder")}
          value={apiKeyValue}
          onChange={(e) => setApiKeyValue(e.target.value)}
          className="font-mono text-sm"
        />
        {isEditing && editing?.api_key && (
          <p className="text-2xs text-muted-foreground-subtle font-mono">
            {t("providers.field_api_key_current", { key: editing.api_key })}
          </p>
        )}
        <p className="text-2xs text-muted-foreground-subtle">{t("providers.field_api_key_vault_hint")}</p>
      </div>

      {/* Notes */}
      <Field label={`${t("providers.field_notes")} (${t("providers.optional")})`} labelClassName="text-xs">
        <Textarea
          placeholder={t("providers.field_notes_placeholder")}
          value={form.notes ?? ""}
          onChange={(e) => setForm((f) => ({ ...f, notes: e.target.value }))}
          className="text-sm resize-none h-16"
        />
      </Field>
      </>)}

      {activeTab === "advanced" && (<>
      {/* Timeouts (request_secs is the main TTS knob — long synth +
          voice-clone warmup can exceed the 120s default) */}
      <TimeoutsSection
        value={getOpts(form).timeouts ?? {}}
        onChange={(timeouts) =>
          setForm((f) => ({ ...f, options: { ...getOpts(f), timeouts } }))
        }
      />

      {/* Enabled */}
      <div className="flex items-center gap-2">
        <Switch
          checked={form.enabled ?? true}
          onCheckedChange={(v) => setForm((f) => ({ ...f, enabled: v }))}
          aria-label={t("providers.field_enabled")}
        />
        <span className="text-xs font-medium text-muted-foreground">
          {t("providers.field_enabled")}
        </span>
      </div>
      </>)}
    </>
  );
}