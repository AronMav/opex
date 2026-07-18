"use client";

import { useTranslation } from "@/hooks/use-translation";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useTtsVoices } from "@/lib/queries";

const SERVER_DEFAULT = "__default__";

export interface VoiceSelectProps {
  value: string;
  onChange: (voiceId: string) => void;
  providerName: string;
  /** Adds a "— server default" item mapping to "" (provider dialog semantics:
   *  unset voice = the TTS server's own default). */
  allowServerDefault?: boolean;
  disabled?: boolean;
  size?: "sm" | "default";
  className?: string;
  id?: string;
}

/** Unified TTS voice picker over GET /api/tts/voices. Degrades to a free-text
 *  input when the list is unavailable (toolgate down, provider without a voice
 *  listing) so the field always stays fillable. */
export function VoiceSelect({
  value,
  onChange,
  providerName,
  allowServerDefault = false,
  disabled,
  size = "default",
  className,
  id,
}: VoiceSelectProps) {
  const { t } = useTranslation();
  const { data: voices = [], isLoading, isError } = useTtsVoices(providerName || null);

  if (!isLoading && (isError || voices.length === 0)) {
    return (
      <Input
        id={id}
        value={value}
        disabled={disabled || !providerName}
        placeholder={t("profiles.voice_placeholder")}
        onChange={(e) => onChange(e.target.value)}
        className={`font-mono text-sm ${className ?? ""}`}
      />
    );
  }

  return (
    <Select
      value={value === "" ? (allowServerDefault ? SERVER_DEFAULT : "") : value}
      onValueChange={(v) => onChange(v === SERVER_DEFAULT ? "" : v)}
      disabled={disabled || !providerName}
    >
      <SelectTrigger id={id} size={size} className={className}>
        <SelectValue placeholder={isLoading ? t("fields.voice_loading") : t("profiles.voice_placeholder")} />
      </SelectTrigger>
      <SelectContent>
        {allowServerDefault && (
          <SelectItem value={SERVER_DEFAULT} className="text-sm text-muted-foreground">
            <span className="text-muted-foreground">&mdash; {t("providers.voice_server_default")}</span>
          </SelectItem>
        )}
        {voices.map((v) => (
          <SelectItem key={v.id} value={v.id} className="text-sm font-mono">
            <span className="flex flex-col">
              <span>{v.name || v.id}</span>
              {(v.language || v.description) && (
                <span className="text-3xs text-muted-foreground-subtle">
                  {[v.language, v.description].filter(Boolean).join(" · ")}
                </span>
              )}
            </span>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
