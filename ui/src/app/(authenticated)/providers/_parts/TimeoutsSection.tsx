"use client";
import * as React from "react";
import type { TimeoutsConfig } from "@/types/api";
import { useTranslation } from "@/hooks/use-translation";

const DEFAULTS: TimeoutsConfig = {
  connect_secs: 10,
  request_secs: 120,
  stream_inactivity_secs: 60,
  stream_max_duration_secs: 600,
};

const BOUNDS: Record<keyof TimeoutsConfig, [number, number]> = {
  connect_secs: [1, 120],
  request_secs: [0, 3600],
  stream_inactivity_secs: [0, 3600],
  stream_max_duration_secs: [0, 7200],
};

type Props = {
  value: Partial<TimeoutsConfig>;
  onChange: (next: Partial<TimeoutsConfig>) => void;
};

export function TimeoutsSection({ value, onChange }: Props) {
  const { t } = useTranslation();

  const fields: Array<[keyof TimeoutsConfig, string]> = [
    ["connect_secs", t("providers.timeout_connect")],
    ["request_secs", t("providers.timeout_request")],
    ["stream_inactivity_secs", t("providers.timeout_stream_inactivity")],
    ["stream_max_duration_secs", t("providers.timeout_stream_max")],
  ];
  const errors: Partial<Record<keyof TimeoutsConfig, string>> = {};
  for (const [k] of fields) {
    const v = value[k] ?? DEFAULTS[k];
    const [lo, hi] = BOUNDS[k];
    if (v < lo || v > hi) errors[k] = t("providers.timeout_error", { lo, hi });
  }

  return (
    <fieldset className="border rounded-md p-3 space-y-2">
      <legend className="text-sm font-medium">
        {t("providers.timeouts_section")}
      </legend>
      {fields.map(([k, label]) => (
        <label key={k} className="flex items-center justify-between gap-4">
          <span className="text-sm">{label}</span>
          <div className="flex items-center gap-2">
            <input
              type="number"
              aria-label={label}
              value={value[k] ?? DEFAULTS[k]}
              onChange={(e) => onChange({ ...value, [k]: Number(e.target.value) })}
              className="w-24 rounded border bg-background px-2 py-1 text-sm"
              min={BOUNDS[k][0]}
              max={BOUNDS[k][1]}
            />
            {errors[k] && (
              <span className="text-xs text-destructive">{errors[k]}</span>
            )}
          </div>
        </label>
      ))}
    </fieldset>
  );
}
