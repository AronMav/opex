"use client";

import { useMemo, useState } from "react";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Input } from "@/components/ui/input";
import { useTranslation } from "@/hooks/use-translation";
import { CRON_PRESETS, TIMEZONES, describeCron, isValidCron } from "@/lib/cron";
import type { CronPreset, TimezoneOption } from "@/lib/cron";

interface CronSchedulePickerProps {
  value: string;
  onChange: (value: string) => void;
  timezone?: string;
  onTimezoneChange?: (tz: string) => void;
  showTimezone?: boolean;
  presets?: CronPreset[];
  timezones?: TimezoneOption[];
  showDescription?: boolean;
  className?: string;
}

const CUSTOM_VALUE = "__custom__";

export function CronSchedulePicker({
  value,
  onChange,
  timezone,
  onTimezoneChange,
  showTimezone = true,
  presets = CRON_PRESETS,
  timezones = TIMEZONES,
  showDescription = true,
  className,
}: CronSchedulePickerProps) {
  const { t } = useTranslation();
  const [forceCustom, setForceCustom] = useState(false);

  const matchesPreset = useMemo(
    () => presets.some((p) => p.value === value),
    [value, presets],
  );

  const isCustom = forceCustom || (!!value && !matchesPreset);

  const selectValue = isCustom ? CUSTOM_VALUE : value;
  const valid = !value || isValidCron(value);
  const description = value && valid ? describeCron(value, t) : null;

  return (
    <div className={className}>
      {/* Row 1: Preset + Timezone selects */}
      <div className={`grid gap-3 ${showTimezone && onTimezoneChange ? "grid-cols-1 sm:grid-cols-2" : ""}`}>
        <Select
          value={selectValue}
          onValueChange={(v) => {
            if (v === CUSTOM_VALUE) {
              setForceCustom(true);
            } else {
              setForceCustom(false);
              onChange(v);
            }
          }}
        >
          <SelectTrigger className="w-full h-9 text-sm">
            <SelectValue placeholder={t("cron.select_schedule")} />
          </SelectTrigger>
          <SelectContent className="max-h-[280px]">
            {presets.map((p) => (
              <SelectItem key={p.value} value={p.value} className="text-sm">
                {t(p.labelKey)}
              </SelectItem>
            ))}
            <SelectItem value={CUSTOM_VALUE} className="text-sm font-medium text-primary">
              {t("cron.preset_custom")}
            </SelectItem>
          </SelectContent>
        </Select>

        {showTimezone && onTimezoneChange && (
          <Select value={timezone || "UTC"} onValueChange={onTimezoneChange}>
            <SelectTrigger className="w-full h-9 text-sm">
              <SelectValue placeholder={t("cron.select_timezone")} />
            </SelectTrigger>
            <SelectContent className="max-h-[280px]">
              {timezones.map((tz) => (
                <SelectItem key={tz.value} value={tz.value} className="text-sm">
                  {tz.labelKey ? t(tz.labelKey) : tz.value}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
      </div>

      {/* Row 2: Custom cron input */}
      {isCustom && (
        <Input
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={t("cron.cron_placeholder")}
          className={`mt-2 h-9 font-mono text-sm ${!valid ? "border-destructive focus-visible:ring-destructive" : ""}`}
        />
      )}

      {/* Validation error */}
      {isCustom && value && !valid && (
        <p className="mt-1 text-xs text-destructive">{t("cron.cron_validation_error")}</p>
      )}

      {/* Row 3: Human-readable description */}
      {showDescription && description && (
        <p className="mt-1.5 font-mono text-2xs text-muted-foreground-subtle">{description}</p>
      )}
    </div>
  );
}
