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
  "data-testid"?: string;
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
  "data-testid": testId,
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
        data-testid={testId}
      />
    );
  }

  // A configured voice id that isn't in the fetched list (voice removed
  // server-side, or a different TTS backend). Radix Select blanks the trigger
  // for an unmatched value, so surface it as a synthetic item.
  const staleValue =
    value !== "" && value !== SERVER_DEFAULT && !voices.some((v) => v.id === value) ? value : null;

  return (
    <Select
      value={value === "" ? (allowServerDefault ? SERVER_DEFAULT : "") : value}
      onValueChange={(v) => onChange(v === SERVER_DEFAULT ? "" : v)}
      disabled={disabled || !providerName}
    >
      <SelectTrigger id={id} size={size} className={className} data-testid={testId}>
        <SelectValue placeholder={isLoading ? t("fields.voice_loading") : t("profiles.voice_placeholder")} />
      </SelectTrigger>
      <SelectContent>
        {allowServerDefault && (
          <SelectItem value={SERVER_DEFAULT} className="text-sm text-muted-foreground">
            <span className="text-muted-foreground">&mdash; {t("providers.voice_server_default")}</span>
          </SelectItem>
        )}
        {staleValue && (
          <SelectItem value={staleValue} className="text-sm font-mono">
            <span className="truncate">{staleValue}</span>
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
